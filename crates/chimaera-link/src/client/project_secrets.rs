//! Every value submission has a fresh passive preflight and one owned send.
use super::*;
use crate::project_secrets::{self as wire, Catalog, Command, Error, Receipt};
use chimaera_core::project_secret_status::BODY_MAX;
use zeroize::Zeroizing;
const ROOT: &[&str] = &["v1", "personal", "project-secrets"];

struct SecretBody(Zeroizing<Vec<u8>>);
impl AsRef<[u8]> for SecretBody {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}
async fn read<T: DeserializeOwned>(
    mut response: reqwest::Response,
) -> std::result::Result<T, Error> {
    let mut bytes = Vec::with_capacity(BODY_MAX);
    while let Some(chunk) = response.chunk().await.map_err(|_| Error::Unconfirmed)? {
        if chunk.len() > BODY_MAX - bytes.len() {
            return Err(Error::Unconfirmed);
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| Error::Unconfirmed)
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Failure {
    version: u16,
    error: String,
}
async fn rejection(response: reqwest::Response) -> Error {
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Error::SignInRequired;
    }
    let status = response.status();
    let Ok(failure) = read::<Failure>(response).await else {
        return Error::Unconfirmed;
    };
    if failure.version != 1 {
        return Error::Unconfirmed;
    }
    match (status.as_u16(), failure.error.as_str()) {
        (404, "unsupported") | (426, "unsupported") => Error::Unsupported,
        (400, "invalid_request") => Error::InvalidRequest,
        (404, "operation_unavailable") => Error::OperationUnavailable,
        (429, "limit_reached") => Error::LimitReached,
        (503, "unavailable") => Error::Unavailable,
        _ => Error::Unconfirmed,
    }
}
impl Client {
    /// Fixed account read; an old account refuses this optional adapter.
    pub async fn personal_control_context(
        &self,
    ) -> std::result::Result<wire::ControlContext, Error> {
        let _slot = self
            .inner
            .secret_requests
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::LimitReached)?;
        let response = self
            .request_raw(
                Method::GET,
                path(&self.inner.account, &["v1", "personal", "control-context"]),
                None,
            )
            .await
            .map_err(|_| Error::Unavailable)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(Error::Unsupported);
        }
        if !response.status().is_success() {
            return Err(rejection(response).await);
        }
        let context: wire::ControlContext = read(response).await?;
        context.validate()?;
        Ok(context)
    }
    /// One canonical page, so account adapters do not manufacture larger pages.
    pub async fn project_secrets_page(
        &self,
        after: Option<&str>,
    ) -> std::result::Result<Catalog, Error> {
        if after.is_some_and(|cursor| !wire::id(cursor)) {
            return Err(Error::InvalidRequest);
        }
        let _slot = self
            .inner
            .secret_requests
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::LimitReached)?;
        self.secret_page_inner(after).await
    }
    async fn secret_page_inner(&self, after: Option<&str>) -> std::result::Result<Catalog, Error> {
        let mut url = path(&self.keeper().await.map_err(|_| Error::Unavailable)?, ROOT);
        if let Some(after) = after {
            url.query_pairs_mut().append_pair("after", after);
        }
        // Read-only capability negotiation retains the usual account-only
        // refresh decision. The value-bearing send below never retries.
        let mut response = None;
        for attempt in 0..2 {
            let token = self
                .access_token()
                .await
                .map_err(|_| Error::SignInRequired)?;
            let answer = self
                .inner
                .http
                .get(url.clone())
                .bearer_auth(&token)
                .send()
                .await
                .map_err(|_| Error::Unavailable)?;
            if answer.status() == reqwest::StatusCode::UNAUTHORIZED
                && attempt == 0
                && self
                    .account_rejects(&token)
                    .await
                    .map_err(|_| Error::Unavailable)?
            {
                self.refresh_if_current(&token)
                    .await
                    .map_err(|_| Error::SignInRequired)?;
                continue;
            }
            response = Some(answer);
            break;
        }
        let response = response.ok_or(Error::SignInRequired)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(Error::Unsupported);
        }
        if !response.status().is_success() {
            return Err(rejection(response).await);
        }
        let page: Catalog = read(response).await?;
        page.validate_after(after).map_err(|_| Error::Unsupported)?;
        Ok(page)
    }
    /// Passive and bounded. Capability is presentation until another fresh
    /// catalog is read by the command's retained owner.
    pub async fn project_secrets_catalog(&self) -> std::result::Result<Vec<Catalog>, Error> {
        let _slot = self
            .inner
            .secret_requests
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::LimitReached)?;
        let mut pages = Vec::new();
        let mut after: Option<String> = None;
        let mut count = 0;
        for _ in 0..4 {
            let page = self.secret_page_inner(after.as_deref()).await?;
            if pages
                .first()
                .is_some_and(|first: &Catalog| first.name_policy != page.name_policy)
            {
                return Err(Error::Unsupported);
            }
            count += page.projects.len();
            if count > 128 {
                return Err(Error::Unsupported);
            }
            after = page.next.clone();
            pages.push(page);
            if after.is_none() {
                return Ok(pages);
            }
        }
        Err(Error::Unsupported)
    }
    /// No retry or value replay, including after 401. The task retains both
    /// client quotas until its actual preflight/send/response work finishes.
    pub async fn project_secret_command(
        &self,
        command: Command,
    ) -> std::result::Result<Receipt, Error> {
        command.validate_shape()?;
        let slot = self
            .inner
            .secret_requests
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::LimitReached)?;
        let writer = self
            .inner
            .secret_writers
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::LimitReached)?;
        let client = self.clone();
        tokio::spawn(async move {
            let (_slot, _writer) = (slot, writer);
            tokio::time::timeout(
                Duration::from_secs(30),
                client.secret_command_inner(command),
            )
            .await
            .map_err(|_| Error::Unconfirmed)?
        })
        .await
        .map_err(|_| Error::Unconfirmed)?
    }
    async fn secret_command_inner(&self, command: Command) -> std::result::Result<Receipt, Error> {
        // This read is bounded independently; reserving the writer cannot
        // borrow the mutation send helper's automatic bearer replay behavior.
        let pages = self.project_secrets_catalog().await?;
        let page = pages
            .iter()
            .find(|p| {
                p.projects
                    .iter()
                    .any(|p| p.workspace_id == command.identity().1)
            })
            .ok_or(Error::Unavailable)?;
        let names = command.validate_catalog(page)?;
        let token = self
            .access_token()
            .await
            .map_err(|_| Error::SignInRequired)?;
        let url = path(
            &self.keeper().await.map_err(|_| Error::Unavailable)?,
            &["v1", "personal", "project-secrets", "commands"],
        );
        // Bound before writing, including escaped JSON, without a reallocating
        // secret buffer. Vec capacity remains fixed through serialization.
        struct Bounded(Zeroizing<Vec<u8>>);
        impl std::io::Write for Bounded {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                if b.len() > wire::COMMAND_MAX - self.0.len() {
                    return Err(std::io::ErrorKind::InvalidInput.into());
                }
                self.0.extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut body = Bounded(Zeroizing::new(Vec::with_capacity(wire::COMMAND_MAX)));
        serde_json::to_writer(&mut body, &command).map_err(|_| Error::InvalidRequest)?;
        let response = self
            .inner
            .http
            .post(url)
            .bearer_auth(token)
            .header("content-type", "application/json")
            .body(bytes::Bytes::from_owner(SecretBody(body.0)))
            .send()
            .await
            .map_err(|_| Error::Unconfirmed)?;
        if response.status() == reqwest::StatusCode::CONFLICT {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Conflict {
                version: u16,
                error: String,
                #[serde(deserialize_with = "required")]
                project: Option<wire::Project>,
            }
            fn required<'de, D: serde::Deserializer<'de>>(
                d: D,
            ) -> std::result::Result<Option<wire::Project>, D::Error> {
                serde::Deserialize::deserialize(d)
            }
            let reply: Conflict = read(response).await?;
            if reply.version != 1
                || reply.error != "state_changed"
                || reply.project.as_ref().is_some_and(|p| {
                    p.validate().is_err() || p.workspace_id != command.identity().1
                })
            {
                return Err(Error::Unconfirmed);
            }
            return Err(Error::StateChanged);
        }
        if !response.status().is_success() {
            return Err(rejection(response).await);
        }
        let receipt: Receipt = read(response).await?;
        command.validate_receipt(&receipt, &names)?;
        Ok(receipt)
    }
    /// Reads one original operation; never creates an intent or submits values.
    pub async fn project_secret_operation(
        &self,
        operation: &str,
    ) -> std::result::Result<Receipt, Error> {
        if !wire::uuid(operation) {
            return Err(Error::InvalidRequest);
        }
        let _slot = self
            .inner
            .secret_requests
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::LimitReached)?;
        let response = self
            .keeper_request_raw(
                Method::GET,
                &["v1", "personal", "project-secrets", "operations", operation],
                None,
            )
            .await
            .map_err(|_| Error::Unavailable)?;
        if !response.status().is_success() {
            return Err(rejection(response).await);
        }
        let receipt: Receipt = read(response).await?;
        receipt.validate().map_err(|_| Error::Unconfirmed)?;
        if receipt.operation_id != operation {
            return Err(Error::Unconfirmed);
        }
        Ok(receipt)
    }
}
