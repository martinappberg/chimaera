//! Forwarded viewer requests are bound to one registered project and live epoch.
//! The authenticated gateway chooses placement; the target resolves resources.
use crate::AppState;
use anyhow::{ensure, Context, Result};
use axum::{
    body::Body,
    extract::{Query, State},
    http::{HeaderMap, Method, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Component, Path},
    sync::Arc,
};

pub(crate) const WORKSPACE_HEADER: &str = "x-chimaera-workspace";
pub(crate) const EPOCH_HEADER: &str = "x-chimaera-epoch";
mod commands;
pub(crate) mod files;
pub(crate) mod paths;
const MAX_JSON: usize = 1024 * 1024;

/// Captured at request admission, consumed only at the actual mutation commit.
/// A body/queue wait cannot silently adopt a replacement account generation.
#[derive(Clone)]
pub(crate) struct Mutation {
    scope: Scope,
    generation: u64,
}
impl Mutation {
    pub(crate) fn for_scope(state: &AppState, scope: Scope) -> Result<Self> {
        let admission = Self {
            scope,
            generation: crate::pro::mutation::generation(state),
        };
        // Prove the captured generation as well as the supplied epoch before
        // exposing it to a long-lived socket; neither may refresh implicitly.
        admission.validate(state)?;
        Ok(admission)
    }
    /// Reads prove the captured identity without consuming mutation capacity.
    /// Writes still call begin() at commit/dispatch for atomic reservation.
    pub(crate) fn validate(&self, state: &AppState) -> Result<()> {
        if self.generation != crate::pro::mutation::generation(state) {
            return Err(crate::pro::mutation::Changed.into());
        }
        self.scope
            .validate(state)
            .map_err(|_| crate::pro::mutation::Changed)?;
        if self.generation != crate::pro::mutation::generation(state) {
            return Err(crate::pro::mutation::Changed.into());
        }
        Ok(())
    }
    pub(crate) fn session(&self, state: &AppState, session: &str) -> Result<()> {
        self.scope
            .session(state, session)
            .map_err(|_| crate::pro::mutation::Changed.into())
    }
    pub(crate) fn capture(state: &AppState, workspace: &str) -> Result<Option<Self>> {
        let Some((epoch, generation)) = crate::pro::mutation::capture(state, workspace)? else {
            return Ok(None);
        };
        Ok(Some(Self {
            scope: Scope {
                workspace_id: workspace.into(),
                epoch,
                viewer_root: None,
            },
            generation,
        }))
    }
    pub(crate) fn begin(&self, state: &AppState) -> Result<crate::policy::Reservation> {
        self.scope
            .validate(state)
            .map_err(|_| crate::pro::mutation::Changed)?;
        crate::pro::mutation::begin(
            state,
            &self.scope.workspace_id,
            self.scope.epoch,
            self.generation,
        )
        .map(crate::policy::Reservation::new)
    }
}
pub(crate) fn begin_mutation(
    state: &AppState,
    mutation: &Option<Extension<Mutation>>,
) -> Result<Option<crate::policy::Reservation>> {
    mutation
        .as_ref()
        .map(|Extension(mutation)| mutation.begin(state))
        .transpose()
}
pub(crate) fn mutation_failure(error: &anyhow::Error) -> Option<Response> {
    error
        .chain()
        .any(|cause| {
            cause.is::<crate::pro::mutation::Changed>()
                || cause
                    .downcast_ref::<std::io::Error>()
                    .and_then(std::io::Error::get_ref)
                    .is_some_and(|inner| inner.is::<crate::pro::mutation::Changed>())
        })
        .then(|| denied(StatusCode::CONFLICT))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Scope {
    pub workspace_id: String,
    pub epoch: u64,
    pub viewer_root: Option<String>,
}
#[derive(Default, Deserialize)]
pub(crate) struct Fields {
    pub workspace_id: Option<String>,
    pub epoch: Option<u64>,
    pub viewer_root: Option<String>,
}
impl Fields {
    pub(crate) fn scope(self) -> Result<Option<Scope>> {
        match (self.workspace_id, self.epoch) {
            (None, None) if self.viewer_root.is_none() => Ok(None),
            (Some(workspace_id), Some(epoch)) if valid_id(&workspace_id) && epoch > 0 => {
                Ok(Some(Scope {
                    workspace_id,
                    epoch,
                    viewer_root: self.viewer_root,
                }))
            }
            _ => anyhow::bail!("invalid workspace scope"),
        }
    }
}
fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
}
pub(crate) fn from_headers(headers: &HeaderMap) -> Result<Option<Scope>> {
    let one = |name| -> Result<Option<&str>> {
        let values = headers.get_all(name);
        ensure!(values.iter().count() <= 1, "ambiguous workspace scope");
        values
            .iter()
            .next()
            .map(|v| v.to_str().context("invalid workspace scope"))
            .transpose()
    };
    let workspace_id = one(WORKSPACE_HEADER)?.map(str::to_owned);
    let epoch = one(EPOCH_HEADER)?
        .map(|value| {
            ensure!(
                !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()),
                "invalid workspace epoch"
            );
            value.parse::<u64>().context("invalid workspace epoch")
        })
        .transpose()?;
    Fields {
        workspace_id,
        epoch,
        viewer_root: one(paths::HEADER)?.map(str::to_owned),
    }
    .scope()
}
impl Scope {
    pub(crate) fn alias(&self, state: &AppState) -> Result<Option<paths::Alias>> {
        let root = crate::lock(&state.workspaces)
            .get(&self.workspace_id)
            .context("unknown workspace")?
            .root;
        self.viewer_root
            .as_deref()
            .map(|encoded| paths::Alias::decode(encoded, root))
            .transpose()
    }
    pub(crate) fn validate(&self, state: &AppState) -> Result<()> {
        ensure!(
            crate::lock(&state.workspaces)
                .get(&self.workspace_id)
                .is_some(),
            "unknown workspace"
        );
        self.alias(state)?;
        crate::pro::validate_execution_scope(state, &self.workspace_id, self.epoch)
    }
    /// A scope this machine cannot admit yet only because it just thawed and
    /// its own renewal of exactly this epoch is still out (see
    /// `pro::scope_renewing`): wait for that renewal rather than refusing.
    pub(crate) fn renewing(&self, state: &AppState) -> bool {
        crate::pro::scope_renewing(state, &self.workspace_id, self.epoch)
    }
    /// Waits (bounded by the resume window) for that renewal; `true` once a
    /// fresh proof exists. Admits nothing: the caller validates again.
    pub(crate) async fn await_renewal(&self, state: &AppState) -> bool {
        crate::pro::await_scope_renewal(state, &self.workspace_id, self.epoch).await
    }
    pub(crate) fn session(&self, state: &AppState, session: &str) -> Result<()> {
        ensure!(
            crate::lock(&state.session_workspaces).get(session) == Some(&self.workspace_id),
            "session belongs to another workspace"
        );
        self.validate(state)
    }
    pub(crate) async fn paths(&self, state: &AppState, paths: Vec<String>) -> Result<()> {
        ensure!(
            paths.len() <= 2048 && paths.iter().all(|p| p.len() <= 4096),
            "path budget exceeded"
        );
        let root = crate::lock(&state.workspaces)
            .get(&self.workspace_id)
            .context("unknown workspace")?
            .root;
        blocking_paths(&crate::fs::FILESYSTEM_WORK, move || {
            let root = root.canonicalize()?;
            for path in paths {
                ensure!(within(&root, &path)?, "path outside workspace");
            }
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        Ok(())
    }
    /// Which of `paths` a viewer of this project may READ: anything inside the
    /// project, plus the images its own conversations saved (a chat's pasted
    /// pictures live in the owner's uploads folder, outside the project).
    /// Writes keep the strict [`Scope::paths`] rule.
    pub(crate) async fn readable(&self, state: &AppState, paths: Vec<String>) -> Result<Vec<bool>> {
        ensure!(
            paths.len() <= 2048 && paths.iter().all(|p| p.len() <= 4096),
            "path budget exceeded"
        );
        let root = crate::lock(&state.workspaces)
            .get(&self.workspace_id)
            .context("unknown workspace")?
            .root;
        let uploads: Vec<std::path::PathBuf> = crate::lock(&state.session_workspaces)
            .iter()
            .filter(|(_, workspace)| **workspace == self.workspace_id)
            .map(|(session, _)| state.uploads_root.join(session))
            .collect();
        blocking_paths(&crate::fs::FILESYSTEM_WORK, move || {
            let root = root.canonicalize()?;
            // A conversation that never saved an image has no folder.
            let uploads: Vec<_> = uploads
                .iter()
                .filter_map(|dir| dir.canonicalize().ok())
                .collect();
            Ok::<_, anyhow::Error>(
                paths
                    .iter()
                    .map(|path| {
                        within(&root, path).unwrap_or(false)
                            || uploads.iter().any(|dir| within(dir, path).unwrap_or(false))
                    })
                    .collect(),
            )
        })
        .await?
    }
    async fn read(&self, state: &AppState, path: String) -> Result<()> {
        if self.readable(state, vec![path.clone()]).await?.first() == Some(&true) {
            return Ok(());
        }
        // Whether this machine has anything at that path decides who else may
        // answer: nothing here means the viewer's own computer can (its own
        // file); something here means the viewer must not show a different
        // file that happens to share the path. Contents never leave.
        let missing = blocking_paths(&crate::fs::FILESYSTEM_WORK, move || {
            crate::fs::expand_tilde(&path).is_ok_and(|path| {
                matches!(std::fs::symlink_metadata(path), Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound)
            })
        })
        .await?;
        Err(Outside { missing }.into())
    }
}
/// The blocking job owns capacity even when its HTTP observer disappears.
async fn blocking_paths<T: Send + 'static>(
    limiter: &'static tokio::sync::Semaphore,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T> {
    let permit = limiter.acquire().await?;
    Ok(tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await?)
}

/// A refused scoped request. A read outside the project: `not_found` lets
/// the viewer's own computer answer with its own file; `outside_project`
/// tells it this machine has a different one it must not stand in for.
fn outside_read(error: anyhow::Error) -> Response {
    match error.downcast_ref::<Outside>() {
        Some(Outside { missing: true }) => {
            (StatusCode::NOT_FOUND, Json(json!({"error":"not_found"}))).into_response()
        }
        Some(Outside { missing: false }) => (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"outside_project"})),
        )
            .into_response(),
        None => denied(StatusCode::FORBIDDEN),
    }
}
/// A viewer's read of a path outside what it may read here.
#[derive(Debug)]
struct Outside {
    /// Nothing exists at that path on this machine.
    missing: bool,
}
impl std::fmt::Display for Outside {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("path outside workspace")
    }
}
impl std::error::Error for Outside {}
/// Resolve existing symlinks and the nearest existing parent of a new file.
/// `..` is rejected before walking nonexistent parents, so it cannot escape later.
fn within(root: &Path, raw: &str) -> Result<bool> {
    let path = crate::fs::expand_tilde(raw)?;
    ensure!(path.is_absolute(), "invalid scoped path");
    if path.components().any(|p| p == Component::ParentDir) {
        // Existing relative document links are checked with their actual
        // symlink/parent semantics. New traversal paths remain refused.
        return Ok(path.canonicalize()?.starts_with(root));
    }
    let mut existing = path.as_path();
    loop {
        match existing.canonicalize() {
            Ok(real) => return Ok(real.starts_with(root)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // A dangling symlink is not a new path under its parent.
                if std::fs::symlink_metadata(existing).is_ok() {
                    return Ok(false);
                }
                existing = existing.parent().context("no existing parent")?;
            }
            Err(error) => return Err(error.into()),
        }
    }
}
fn denied(status: StatusCode) -> Response {
    (status, Json(json!({"error":"workspace_scope_changed"}))).into_response()
}
fn same(scope: &Scope, value: Option<&str>) -> Result<()> {
    ensure!(
        value == Some(scope.workspace_id.as_str()),
        "workspace mismatch"
    );
    Ok(())
}

/// Applied after bearer authentication and before the local placement proxy.
/// Unknown forwarded routes fail closed; ordinary direct requests are unchanged.
pub(crate) async fn reject_unbound(request: Request<Body>, next: Next) -> Response {
    let path = request.uri().path();
    if (path.starts_with("/api/v1/mcp/")
        || path.starts_with("/api/v1/agent-events/")
        || path.starts_with("/proxy/"))
        && [WORKSPACE_HEADER, EPOCH_HEADER, paths::HEADER]
            .iter()
            .any(|name| request.headers().contains_key(*name))
    {
        return denied(StatusCode::FORBIDDEN);
    }
    next.run(request).await
}
pub(crate) async fn middleware(
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let scope = match from_headers(request.headers()) {
        Ok(None) => return next.run(request).await,
        Ok(Some(scope)) => scope,
        Err(_) => return denied(StatusCode::BAD_REQUEST),
    };
    let mut response = scoped_request(state, scope.clone(), request, next).await;
    response
        .headers_mut()
        .insert("x-chimaera-scope-version", "1".parse().unwrap());
    response
        .headers_mut()
        .insert(WORKSPACE_HEADER, scope.workspace_id.parse().unwrap());
    response
        .headers_mut()
        .insert(EPOCH_HEADER, scope.epoch.to_string().parse().unwrap());
    response
}
async fn scoped_request(
    state: Arc<AppState>,
    scope: Scope,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let mut generation = crate::pro::mutation::generation(&state);
    if scope.validate(&state).is_err() {
        // The request that woke a suspended owner (or any reaching it while
        // it renews its own epoch) is answered after that renewal, not 409.
        if !(scope.renewing(&state) && scope.await_renewal(&state).await) {
            return denied(StatusCode::CONFLICT);
        }
        generation = crate::pro::mutation::generation(&state);
        if scope.validate(&state).is_err() {
            return denied(StatusCode::CONFLICT);
        }
    }
    let path = request
        .uri()
        .path()
        .strip_prefix("/api/v1")
        .unwrap_or(request.uri().path())
        .to_owned();
    let method = request.method().clone();
    let read = method == Method::GET || method == Method::HEAD;
    let alias = match scope.alias(&state) {
        Ok(alias) => alias,
        Err(_) => return denied(StatusCode::BAD_REQUEST),
    };
    let filesystem = if path.starts_with("/fs/") || path.starts_with("/git/") {
        let owner = state.clone();
        let binding = scope.clone();
        match blocking_paths(&crate::fs::FILESYSTEM_WORK, move || {
            files::Context::pin(&owner, binding, generation)
        })
        .await
        {
            Ok(Ok(filesystem)) => Some(filesystem),
            _ => return denied(StatusCode::CONFLICT),
        }
    } else {
        None
    };
    let mut query: HashMap<String, String> =
        match Query::<Vec<(String, String)>>::try_from_uri(request.uri()) {
            Ok(Query(values)) if values.len() <= 64 => {
                let mut unique = HashMap::new();
                for (key, value) in values {
                    if unique.insert(key, value).is_some() {
                        return denied(StatusCode::BAD_REQUEST);
                    }
                }
                unique
            }
            _ => return denied(StatusCode::BAD_REQUEST),
        };
    if let Some(alias) = &alias {
        if path.starts_with("/fs/") || path.starts_with("/git/") {
            for key in ["path", "dir", "repo", "orig"] {
                if let Some(value) = query.get_mut(key) {
                    *value = alias.input(value);
                }
            }
            let encoded = paths::encode_query(
                &query
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<Vec<_>>(),
            );
            let uri = format!(
                "{}{}{}",
                request.uri().path(),
                if encoded.is_empty() { "" } else { "?" },
                encoded
            );
            match uri.parse() {
                Ok(uri) => *request.uri_mut() = uri,
                Err(_) => return denied(StatusCode::BAD_REQUEST),
            }
        }
    }
    if path == "/workspaces" && read {
        let workspace = crate::lock(&state.workspaces).get(&scope.workspace_id);
        let mut value = json!([workspace]);
        if let Some(alias) = &alias {
            alias.response("/workspaces", &mut value);
        }
        return Json(value).into_response();
    }
    if path == "/sessions" && read {
        state.wait_restored().await;
        if scope.validate(&state).is_err() {
            return denied(StatusCode::CONFLICT);
        }
        return Json(json!(sessions(&state, &scope))).into_response();
    }
    if path == "/links" && read {
        return Json(json!(links(&state, &scope))).into_response();
    }
    if path == "/fs/drafts" && read {
        let response = crate::drafts::list_drafts(State(state.clone())).await;
        if !response.status().is_success() {
            return response;
        }
        let Ok(bytes) = axum::body::to_bytes(response.into_body(), MAX_JSON).await else {
            return denied(StatusCode::BAD_GATEWAY);
        };
        let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) else {
            return denied(StatusCode::BAD_GATEWAY);
        };
        let Some(rows) = value["drafts"].as_array_mut() else {
            return denied(StatusCode::BAD_GATEWAY);
        };
        let mut kept = Vec::new();
        for row in rows.drain(..).take(64) {
            if let Some(path) = row["path"].as_str() {
                if scope.paths(&state, vec![path.to_owned()]).await.is_ok() {
                    kept.push(row);
                }
            }
        }
        *rows = kept;
        if scope.validate(&state).is_err() {
            return denied(StatusCode::CONFLICT);
        }
        if let Some(alias) = &alias {
            alias.response("/fs/drafts", &mut value);
        }
        return Json(value).into_response();
    }
    // The reading view's checker, for a viewer of this project: root-relative
    // links resolve at the project's own folder (the viewer's `root` names a
    // folder on another machine and is ignored), and no link target outside
    // the project is stat-ed or read.
    if path == "/fs/check_document" && read {
        let Some(document) = query.get("path").cloned() else {
            return denied(StatusCode::BAD_REQUEST);
        };
        let generation = crate::pro::mutation::generation(&state);
        let Some(root) = crate::lock(&state.workspaces)
            .get(&scope.workspace_id)
            .map(|workspace| workspace.root)
        else {
            return denied(StatusCode::CONFLICT);
        };
        // Pin the project before the separate path proof. Every subsequent
        // document/target operation is relative to this exact directory inode.
        let pinned = match crate::doc_check::pin_project(root.clone()).await {
            Ok(pinned) => pinned,
            Err(_) => return denied(StatusCode::CONFLICT),
        };
        if let Err(error) = scope.read(&state, document.clone()).await {
            if generation != crate::pro::mutation::generation(&state)
                || scope.validate(&state).is_err()
            {
                return denied(StatusCode::CONFLICT);
            }
            return outside_read(error);
        }
        let response = crate::doc_check::check_within(document, pinned).await;
        if generation != crate::pro::mutation::generation(&state)
            || scope.validate(&state).is_err()
            || crate::lock(&state.workspaces)
                .get(&scope.workspace_id)
                .is_none_or(|workspace| workspace.root != root)
        {
            return denied(StatusCode::CONFLICT);
        }
        return response;
    }
    if path == "/fs/home" && read {
        let root = crate::lock(&state.workspaces)
            .get(&scope.workspace_id)
            .map(|w| w.root);
        let mut value = json!({"path":root});
        if let Some(alias) = &alias {
            alias.response("/fs/home", &mut value);
        }
        return Json(value).into_response();
    }
    let needs_body = matches!(
        (method.as_str(), path.as_str()),
        (
            "POST",
            "/sessions"
                | "/fs/ticket"
                | "/fs/mkdir"
                | "/fs/create"
                | "/fs/rename"
                | "/fs/copy"
                | "/fs/move"
                | "/fs/delete"
                | "/fs/validate"
                | "/fs/resolve_targets"
        ) | ("PUT", "/links" | "/fs/drafts")
    );
    let mut target_keys = HashMap::new();
    let (mut parts, stream) = request.into_parts();
    let (mut body, stream) = if needs_body {
        let bytes = match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            axum::body::to_bytes(stream, MAX_JSON),
        )
        .await
        {
            Ok(Ok(bytes)) => bytes,
            Ok(Err(_)) => return denied(StatusCode::PAYLOAD_TOO_LARGE),
            Err(_) => return denied(StatusCode::REQUEST_TIMEOUT),
        };
        let mut body = match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(body)) => Value::Object(body),
            _ => return denied(StatusCode::BAD_REQUEST),
        };
        if let Some(alias) = &alias {
            if path.starts_with("/fs/") {
                target_keys = alias.request_body(&mut body);
            }
        }
        (body, None)
    } else {
        (Value::Null, Some(stream))
    };
    // Validation may narrow a compound body (unreadable candidates dropped),
    // so the forwarded body is built from what it approved.
    if let Err(error) = validate_resource(&state, &scope, &method, &path, &query, &mut body).await {
        return outside_read(error);
    }
    let mut request = match stream {
        Some(stream) => Request::from_parts(parts, stream),
        None => {
            parts.headers.remove(axum::http::header::CONTENT_LENGTH);
            Request::from_parts(
                parts,
                Body::from(serde_json::to_vec(&body).unwrap_or_default()),
            )
        }
    };
    // Recheck after filesystem/JSON work; a lease may have changed while awaiting.
    if scope.validate(&state).is_err() {
        return denied(StatusCode::CONFLICT);
    }
    request.extensions_mut().insert(Mutation {
        scope: scope.clone(),
        generation,
    });
    request.extensions_mut().insert(scope);
    if let Some(filesystem) = filesystem {
        request.extensions_mut().insert(filesystem);
    }
    let response = if commands::reserved(&method, &path) {
        commands::run(state, request, next).await
    } else {
        next.run(request).await
    };
    let rewrite = matches!(
        path.as_str(),
        "/fs/list"
            | "/fs/dirs"
            | "/fs/mkdir"
            | "/fs/create"
            | "/fs/rename"
            | "/fs/copy"
            | "/fs/move"
            | "/fs/draft"
            | "/fs/drafts"
            | "/fs/validate"
            | "/fs/resolve_targets"
    );
    if let Some(alias) = alias.filter(|_| rewrite && response.status().is_success()) {
        let (mut parts, body) = response.into_parts();
        let Ok(bytes) = axum::body::to_bytes(body, MAX_JSON).await else {
            return denied(StatusCode::BAD_GATEWAY);
        };
        let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) else {
            return denied(StatusCode::BAD_GATEWAY);
        };
        paths::Alias::restore_keys(&mut value, &target_keys);
        alias.response(&path, &mut value);
        parts.headers.remove(axum::http::header::CONTENT_LENGTH);
        return Response::from_parts(
            parts,
            Body::from(serde_json::to_vec(&value).unwrap_or_default()),
        );
    }
    response
}
async fn validate_resource(
    state: &AppState,
    scope: &Scope,
    method: &Method,
    path: &str,
    query: &HashMap<String, String>,
    body: &mut Value,
) -> Result<()> {
    let read = *method == Method::GET || *method == Method::HEAD;
    if read
        && matches!(
            path,
            "/health" | "/settings" | "/agents" | "/plugins" | "/compute" | "/update"
        )
    {
        return Ok(());
    }
    if let Some(rest) = path.strip_prefix("/workspaces/") {
        let (id, tail) = rest.split_once('/').unwrap_or((rest, ""));
        same(scope, Some(id))?;
        ensure!(
            matches!(
                (method.as_str(), tail),
                ("POST", "open")
                    | (
                        "GET",
                        "timeline" | "plugins" | "agent-plugins" | "skills" | "knowledge"
                    )
                    | ("PUT" | "DELETE", "mastermind")
            ) || (method == Method::POST
                && tail.starts_with("timeline/")
                && tail.ends_with("/deliver")),
            "route unavailable"
        );
        return Ok(());
    }
    if let Some(rest) = path.strip_prefix("/sessions/") {
        let (id, tail) = rest.split_once('/').unwrap_or((rest, ""));
        ensure!(
            valid_id(id)
                && matches!(
                    (method.as_str(), tail),
                    ("DELETE" | "PATCH", "")
                        | ("GET", "journal")
                        | (
                            "POST",
                            "exec" | "upload" | "view" | "rewind" | "fork" | "resume"
                        )
                ),
            "route unavailable"
        );
        return scope.session(state, id);
    }
    if path == "/sessions" && method == Method::POST {
        same(scope, body["workspace_id"].as_str())?;
        if let Some(resume) = body["resume"].as_str() {
            let kind = body["agent"].as_str().unwrap_or("claude");
            let known = crate::lock(&state.recents)
                .list(&scope.workspace_id)
                .iter()
                .any(|row| row.resume.as_deref() == Some(resume) && row.kind.as_str() == kind);
            let root = crate::lock(&state.workspaces)
                .get(&scope.workspace_id)
                .context("workspace missing")?
                .root;
            let directory = state
                .claude_projects_dir
                .join(crate::launcher::encode_cwd(&root));
            let resume = resume.to_owned();
            let scanned = if !known && kind == "claude" {
                tokio::task::spawn_blocking(move || {
                    crate::launcher::scan_resumables(&directory, &[])
                        .iter()
                        .any(|row| row["id"].as_str() == Some(resume.as_str()))
                })
                .await?
            } else {
                false
            };
            ensure!(
                known || scanned,
                "conversation belongs to another workspace"
            );
        }
        return Ok(());
    }
    if let Some(id) = path.strip_prefix("/links/") {
        ensure!(
            *method == Method::DELETE && valid_id(id),
            "route unavailable"
        );
        return scope.session(state, id);
    }
    if path == "/links" && method == Method::PUT {
        scope.session(
            state,
            body["terminal_id"].as_str().context("missing terminal")?,
        )?;
        return scope.session(state, body["agent_id"].as_str().context("missing agent")?);
    }
    if read && matches!(path, "/recents" | "/fs/quickopen") {
        return same(scope, query.get("workspace_id").map(String::as_str));
    }
    if read
        && matches!(
            path,
            "/git/status"
                | "/git/diff"
                | "/git/worktrees"
                | "/git/repos"
                | "/git/branches"
                | "/git/log"
                | "/git/show"
                | "/git/compare"
        )
    {
        same(scope, query.get("workspace_id").map(String::as_str))?;
        let paths = ["repo", "path", "orig"]
            .into_iter()
            .filter_map(|key| query.get(key).cloned())
            .collect();
        return scope.paths(state, paths).await;
    }
    if let Some(key) = path.strip_prefix("/view-state/") {
        ensure!(
            matches!(method.as_str(), "GET" | "PUT")
                && valid_id(key)
                && key.ends_with(&format!("_{}", scope.workspace_id)),
            "view belongs to another workspace"
        );
        return Ok(());
    }
    // Reads may also open the images this project's own conversations saved.
    if read
        && matches!(
            path,
            "/fs/dirs"
                | "/fs/list"
                | "/fs/file"
                | "/fs/markdown"
                | "/fs/table"
                | "/fs/xlsx"
                | "/fs/notebook"
                | "/fs/draft"
                | "/fs/check_document"
        )
    {
        return scope
            .read(state, query.get("path").context("path missing")?.clone())
            .await;
    }
    if method == Method::POST && path == "/fs/ticket" {
        return scope
            .read(
                state,
                body["path"].as_str().context("path missing")?.to_owned(),
            )
            .await;
    }
    if (method == Method::PUT && path == "/fs/file")
        || (method == Method::DELETE && path == "/fs/draft")
        || (method == Method::POST && path == "/fs/upload")
    {
        return scope
            .paths(
                state,
                vec![query
                    .get(if path == "/fs/upload" { "dir" } else { "path" })
                    .context("path missing")?
                    .clone()],
            )
            .await;
    }
    if method == Method::POST && matches!(path, "/fs/mkdir" | "/fs/create" | "/fs/delete")
        || method == Method::PUT && path == "/fs/drafts"
    {
        return scope
            .paths(
                state,
                vec![body["path"].as_str().context("path missing")?.to_owned()],
            )
            .await;
    }
    if method == Method::POST && matches!(path, "/fs/rename" | "/fs/copy" | "/fs/move") {
        return scope
            .paths(
                state,
                vec![
                    body["from"].as_str().context("source missing")?.to_owned(),
                    body["to"]
                        .as_str()
                        .context("destination missing")?
                        .to_owned(),
                ],
            )
            .await;
    }
    // Compound resolvers read many candidates at once, and a conversation's
    // links legitimately include paths outside the project (a pasted image,
    // a file on the viewer's own machine). Every candidate and base still
    // stays inside what this viewer may read: the others are dropped and
    // resolve as unknown, instead of refusing the whole batch.
    if method == Method::POST && matches!(path, "/fs/validate" | "/fs/resolve_targets") {
        return keep_readable_candidates(state, scope, path, body).await;
    }
    anyhow::bail!("route unavailable for workspace viewer")
}
async fn keep_readable_candidates(
    state: &AppState,
    scope: &Scope,
    path: &str,
    body: &mut Value,
) -> Result<()> {
    if !body["workspace_id"].is_null() {
        same(scope, body["workspace_id"].as_str())?;
    }
    let base = body["base"].as_str().context("base missing")?;
    let mut bases = vec![base.to_owned()];
    if let Some(more) = body["bases"].as_array() {
        for p in more {
            bases.push(p.as_str().context("invalid base")?.to_owned());
        }
    }
    let key = if path == "/fs/validate" {
        "candidates"
    } else {
        "targets"
    };
    let candidates = body[key].as_array().context("candidates missing")?.clone();
    ensure!(
        candidates.len() <= 200 && bases.len() <= 9,
        "path budget exceeded"
    );
    // Per candidate: whether it is relative, and every path it would read.
    let mut reads: Vec<(bool, Vec<String>)> = Vec::with_capacity(candidates.len());
    for candidate in &candidates {
        let candidate = candidate.as_str().context("invalid candidate")?;
        let candidate = if path == "/fs/resolve_targets" {
            // Not a filesystem target (a URL, a mail link): nothing to read.
            let Some(decoded) = crate::embed::target_path(candidate) else {
                reads.push((false, Vec::new()));
                continue;
            };
            decoded
        } else {
            candidate.to_owned()
        };
        let expanded = crate::fs::expand_tilde(&candidate)?;
        if expanded.is_absolute() {
            reads.push((false, vec![expanded.to_string_lossy().into_owned()]));
            continue;
        }
        let mut paths = Vec::new();
        for base in &bases {
            paths.push(
                Path::new(base)
                    .join(&expanded)
                    .to_string_lossy()
                    .into_owned(),
            );
            if path == "/fs/validate" && body["strict"] != true {
                if let Some(rest) = candidate
                    .strip_prefix("a/")
                    .or_else(|| candidate.strip_prefix("b/"))
                {
                    paths.push(Path::new(base).join(rest).to_string_lossy().into_owned());
                }
            }
        }
        reads.push((true, paths));
    }
    let mut all = bases.clone();
    for (_, paths) in &reads {
        all.extend(paths.iter().cloned());
    }
    let readable = scope.readable(state, all).await?;
    let bases_readable = readable[..bases.len()].iter().all(|ok| *ok);
    let mut cursor = bases.len();
    let mut kept = Vec::new();
    for (candidate, (relative, paths)) in candidates.into_iter().zip(reads) {
        let fine = readable[cursor..cursor + paths.len()].iter().all(|ok| *ok);
        cursor += paths.len();
        if fine && (!relative || bases_readable) {
            kept.push(candidate);
        }
    }
    body[key] = Value::Array(kept);
    if !bases_readable {
        // Only absolute candidates remain; never hand the resolver a base
        // outside the project to fall back on.
        let root = crate::lock(&state.workspaces)
            .get(&scope.workspace_id)
            .context("unknown workspace")?
            .root;
        body["base"] = json!(root);
        if let Some(map) = body.as_object_mut() {
            map.remove("bases");
        }
    }
    Ok(())
}
pub(crate) fn sessions(state: &AppState, scope: &Scope) -> Vec<Value> {
    let alias = scope.alias(state).ok().flatten();
    crate::session_view::sessions_json(state)
        .into_iter()
        .filter(|row| row["workspace_id"].as_str() == Some(&scope.workspace_id))
        .map(|mut row| {
            if let Some(alias) = &alias {
                alias.session(&mut row);
            }
            row
        })
        .collect()
}

pub(crate) fn links(state: &AppState, scope: &Scope) -> Vec<Value> {
    crate::links::links_json(state)
        .into_iter()
        .filter(|row| {
            scope
                .session(state, row["terminal_id"].as_str().unwrap_or_default())
                .is_ok()
                && scope
                    .session(state, row["agent_id"].as_str().unwrap_or_default())
                    .is_ok()
        })
        .collect()
}

/// Ticket routes authenticate through their unguessable ticket, then bind its
/// actual filesystem resource; scope headers alone never mint file authority.
pub(crate) async fn ticket_middleware(
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let scope = match from_headers(request.headers()) {
        Ok(None) => return next.run(request).await,
        Ok(Some(scope)) => scope,
        Err(_) => return denied(StatusCode::BAD_REQUEST),
    };
    let ticket = request.uri().path().split('/').nth(2).unwrap_or_default();
    let path = crate::lock(&state.tickets).lookup(ticket);
    if scope.validate(&state).is_err()
        || match path {
            // A ticket only ever reads.
            Some(path) => scope
                .read(&state, path.to_string_lossy().into_owned())
                .await
                .is_err(),
            None => true,
        }
    {
        return denied(StatusCode::FORBIDDEN);
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelled_path_observer_keeps_capacity_until_its_worker_drains() {
        let limiter = Box::leak(Box::new(tokio::sync::Semaphore::new(1)));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let task = tokio::spawn(blocking_paths(limiter, move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        }));
        started_rx.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(
            limiter.try_acquire().is_err(),
            "cancelled observer must not release blocked filesystem capacity"
        );
        release_tx.send(()).unwrap();
        let _permit = tokio::time::timeout(std::time::Duration::from_secs(5), limiter.acquire())
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn forwarded_scope_requires_one_complete_exact_pair() {
        let mut headers = HeaderMap::new();
        assert_eq!(from_headers(&headers).unwrap(), None);
        headers.insert(WORKSPACE_HEADER, "w-one".parse().unwrap());
        assert!(from_headers(&headers).is_err());
        headers.insert(EPOCH_HEADER, "4".parse().unwrap());
        assert_eq!(from_headers(&headers).unwrap().unwrap().epoch, 4);
        headers.append(EPOCH_HEADER, "5".parse().unwrap());
        assert!(from_headers(&headers).is_err());
        headers.remove(EPOCH_HEADER);
        for invalid in ["0", "-1", "+4", " 4", "18446744073709551616"] {
            headers.insert(EPOCH_HEADER, invalid.parse().unwrap());
            assert!(from_headers(&headers).is_err());
        }
    }
    #[test]
    fn existing_symlink_and_new_path_cannot_escape_registered_root() {
        let root = std::env::temp_dir().join(format!(
            "chimaera-viewer-scope-{}",
            chimaera_core::generate_token()
        ));
        std::fs::create_dir_all(root.join("project")).unwrap();
        std::fs::create_dir_all(root.join("private")).unwrap();
        let project = root.join("project").canonicalize().unwrap();
        assert!(within(&project, &project.join("new/nested/file").to_string_lossy()).unwrap());
        assert!(!within(&project, &root.join("private").to_string_lossy()).unwrap());
        assert!(!within(&project, &project.join("../private").to_string_lossy()).unwrap());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("private"), project.join("link")).unwrap();
            assert!(!within(&project, &project.join("link/new").to_string_lossy()).unwrap());
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
