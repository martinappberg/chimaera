//! Personal requests never fall back to a daemon or resend a mutation.
use super::*;
use crate::providers::{
    self as wire, Attempt, Catalog, CatalogPage, Command, CommandResult, Error, Mode, ModeReply,
    Original,
};
use zeroize::Zeroizing;
const ROOT: &[&str] = &["v1", "personal", "providers"];

async fn read<T: DeserializeOwned>(
    mut response: reqwest::Response,
) -> std::result::Result<T, Error> {
    let mut body = Zeroizing::new(Vec::with_capacity(wire::BODY_MAX));
    while let Some(chunk) = response.chunk().await.map_err(|_| Error::Unconfirmed)? {
        if chunk.len() > wire::BODY_MAX - body.len() {
            return Err(Error::Unconfirmed);
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| Error::Unconfirmed)
}
fn rejection(status: reqwest::StatusCode, operation: bool) -> Error {
    match status.as_u16() {
        400 => Error::InvalidRequest,
        401 | 403 => Error::SignInRequired,
        404 if operation => Error::OperationUnavailable,
        404 | 426 => Error::Unsupported,
        409 => Error::StateChanged,
        429 => Error::LimitReached,
        503 => Error::Unavailable,
        _ => Error::Unconfirmed,
    }
}
struct SecretBody(Zeroizing<Vec<u8>>);
impl AsRef<[u8]> for SecretBody {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}
struct Bounded(Zeroizing<Vec<u8>>);
impl std::io::Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > wire::COMMAND_MAX - self.0.len() {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Client {
    pub async fn personal_provider_mode(&self) -> std::result::Result<ModeReply, Error> {
        let _slot = self
            .inner
            .provider_requests
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::LimitReached)?;
        tokio::time::timeout(Duration::from_secs(30), self.provider_mode_inner())
            .await
            .map_err(|_| Error::Unavailable)?
    }
    async fn provider_mode_inner(&self) -> std::result::Result<ModeReply, Error> {
        let response = self
            .request_raw(
                Method::GET,
                path(
                    &self.inner.account,
                    &["v1", "personal", "providers", "mode"],
                ),
                None,
            )
            .await
            .map_err(|_| Error::Unavailable)?;
        if !response.status().is_success() {
            return Err(rejection(response.status(), false));
        }
        let reply: ModeReply = read(response).await?;
        reply.validate()?;
        Ok(reply)
    }
    async fn provider_context(&self, expected: &str) -> std::result::Result<(), Error> {
        let mode = self.provider_mode_inner().await?;
        if mode.context != expected {
            return Err(Error::ContextChanged);
        }
        if mode.mode != Mode::Personal {
            return Err(Error::Unsupported);
        }
        Ok(())
    }
    async fn provider_context_token(
        &self,
        expected: &str,
        token: &str,
    ) -> std::result::Result<(), Error> {
        // Final mutation validation uses exactly the bearer that will be sent.
        // It cannot refresh/retry while the caller retains the token owner.
        let response = self
            .inner
            .http
            .get(path(
                &self.inner.account,
                &["v1", "personal", "providers", "mode"],
            ))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| Error::Unavailable)?;
        if !response.status().is_success() {
            return Err(rejection(response.status(), false));
        }
        let mode: ModeReply = read(response).await?;
        mode.validate()?;
        if mode.context != expected {
            return Err(Error::ContextChanged);
        }
        if mode.mode != Mode::Personal {
            return Err(Error::Unsupported);
        }
        Ok(())
    }
    pub async fn personal_provider_catalog(&self) -> std::result::Result<CatalogPage, Error> {
        let _slot = self
            .inner
            .provider_requests
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::LimitReached)?;
        tokio::time::timeout(Duration::from_secs(30), async {
            let mode = self.provider_mode_inner().await?;
            if mode.mode != Mode::Personal {
                return Err(Error::Unsupported);
            }
            let catalog = self.provider_catalog_inner().await?;
            self.provider_context(&mode.context).await?;
            Ok(CatalogPage {
                version: 1,
                context: mode.context,
                catalog,
            })
        })
        .await
        .map_err(|_| Error::Unavailable)?
    }
    async fn provider_catalog_inner(&self) -> std::result::Result<Catalog, Error> {
        let response = self
            .keeper_request_raw(Method::GET, ROOT, None)
            .await
            .map_err(|_| Error::Unavailable)?;
        if !response.status().is_success() {
            return Err(rejection(response.status(), false));
        }
        let catalog: Catalog = read(response).await?;
        catalog.validate()?;
        Ok(catalog)
    }
    /// Reserves before spawning. The complete single send survives observer loss;
    /// neither bearer refresh nor uncertain replies ever replay code or intent.
    pub async fn personal_provider_command(
        &self,
        original: Original,
        command: Command,
    ) -> std::result::Result<CommandResult, Error> {
        command.validate_original(&original)?;
        let request = self
            .inner
            .provider_requests
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::LimitReached)?;
        let writer = self
            .inner
            .provider_writers
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::LimitReached)?;
        let client = self.clone();
        tokio::spawn(async move {
            let (_request, _writer) = (request, writer);
            tokio::time::timeout(
                Duration::from_secs(30),
                client.provider_command_inner(original, command),
            )
            .await
            .map_err(|_| Error::Unconfirmed)?
        })
        .await
        .map_err(|_| Error::Unconfirmed)?
    }
    async fn provider_command_inner(
        &self,
        original: Original,
        command: Command,
    ) -> std::result::Result<CommandResult, Error> {
        self.provider_context(&original.context).await?;
        let catalog = self.provider_catalog_inner().await?;
        if catalog.providers_control != original.registration {
            return Err(Error::StateChanged);
        }
        if catalog.connection(original.provider)?.generation
            != original.expected_connection_generation
        {
            return Err(Error::StateChanged);
        }
        if let wire::Action::Submit { .. } | wire::Action::Cancel { .. } = &command.command {
            let attempt = self
                .provider_operation_inner(&original, &original.operation_id)
                .await?;
            if !matches!(
                attempt.phase,
                wire::Phase::Preparing | wire::Phase::Waiting | wire::Phase::Verifying
            ) {
                return Err(Error::StateChanged);
            }
        }
        let url = path(
            &self.keeper().await.map_err(|_| Error::Unavailable)?,
            &["v1", "personal", "providers", "commands"],
        );
        // Resolve before locking (keeper discovery can read/refresh tokens).
        // Account replacement/signout cannot overtake the final exact-bearer
        // check and the actual bounded send after an observer disappears.
        let tokens = self.inner.tokens.clone().lock_owned().await;
        let token = tokens.as_ref().ok_or(Error::SignInRequired)?;
        self.provider_context_token(&original.context, &token.access_token)
            .await?;
        let mut body = Bounded(Zeroizing::new(Vec::with_capacity(wire::COMMAND_MAX)));
        serde_json::to_writer(&mut body, &command).map_err(|_| Error::InvalidRequest)?;
        let response = self
            .inner
            .http
            .post(url)
            .bearer_auth(&token.access_token)
            .header("content-type", "application/json")
            .body(bytes::Bytes::from_owner(SecretBody(body.0)))
            .send()
            .await
            .map_err(|_| Error::Unconfirmed)?;
        if !response.status().is_success() {
            return Err(rejection(response.status(), false));
        }
        let attempt: Attempt = read(response).await?;
        attempt.validate(&original)?;
        self.provider_context_token(&original.context, &token.access_token)
            .await?;
        drop(tokens);
        Ok(CommandResult {
            version: 1,
            context: original.context,
            operation_id: command.operation_id,
            attempt,
        })
    }
    /// Requests exactly the original parent, including after an uncertain child.
    pub async fn personal_provider_operation(
        &self,
        original: &Original,
        requested: &str,
    ) -> std::result::Result<CommandResult, Error> {
        original.validate()?;
        if requested != original.operation_id {
            return Err(Error::InvalidRequest);
        }
        let _slot = self
            .inner
            .provider_requests
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::LimitReached)?;
        tokio::time::timeout(Duration::from_secs(30), async {
            self.provider_context(&original.context).await?;
            let attempt = self.provider_operation_inner(original, requested).await?;
            self.provider_context(&original.context).await?;
            Ok(CommandResult {
                version: 1,
                context: original.context.clone(),
                operation_id: requested.to_owned(),
                attempt,
            })
        })
        .await
        .map_err(|_| Error::Unconfirmed)?
    }
    async fn provider_operation_inner(
        &self,
        original: &Original,
        requested: &str,
    ) -> std::result::Result<Attempt, Error> {
        let response = self
            .keeper_request_raw(
                Method::GET,
                &["v1", "personal", "providers", "operations", requested],
                None,
            )
            .await
            .map_err(|_| Error::Unavailable)?;
        if !response.status().is_success() {
            return Err(rejection(response.status(), true));
        }
        let attempt: Attempt = read(response).await?;
        attempt.validate(original)?;
        Ok(attempt)
    }
}
