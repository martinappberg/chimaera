//! Lifecycle mutations retain one admission through their complete operation.
//! Unlike file bodies, exec commands must wait without reserving ownership;
//! their admission is checked by the PTY engine at actual dispatch instead.
use super::*;
use std::time::Duration;
static REQUESTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(64);

pub(super) fn reserved(method: &Method, path: &str) -> bool {
    !(matches!(*method, Method::GET | Method::HEAD)
        || path.starts_with("/fs/")
        || path.starts_with("/sessions/") && path.ends_with("/upload"))
}

pub(super) async fn run(state: Arc<AppState>, request: Request<Body>, next: Next) -> Response {
    let permit = match REQUESTS.try_acquire() {
        Ok(permit) => permit,
        Err(_) => return denied(StatusCode::SERVICE_UNAVAILABLE),
    };
    let exec = request.uri().path().ends_with("/exec");
    let (parts, stream) = request.into_parts();
    // Match axum's ordinary endpoint body ceiling; scope-validated compound
    // bodies already have their tighter 1 MiB cap. Never reserve a slow upload.
    let body = match tokio::time::timeout(
        Duration::from_secs(30),
        axum::body::to_bytes(stream, 2 * 1024 * 1024),
    )
    .await
    {
        Ok(Ok(body)) => body,
        Ok(Err(_)) => return denied(StatusCode::PAYLOAD_TOO_LARGE),
        Err(_) => return denied(StatusCode::REQUEST_TIMEOUT),
    };
    let Some(admission) = parts.extensions.get::<Mutation>().cloned() else {
        return denied(StatusCode::CONFLICT);
    };
    // Session/link bindings can change while a caller streams its body too.
    let path = parts
        .uri
        .path()
        .strip_prefix("/api/v1")
        .unwrap_or(parts.uri.path());
    // Lifecycle routes never narrow their body (only /fs/ resolvers do, and
    // those are not reserved here), so the forwarded bytes stay as sent.
    let mut value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if validate_resource(
        &state,
        &admission.scope,
        &parts.method,
        path,
        &HashMap::new(),
        &mut value,
    )
    .await
    .is_err()
    {
        return denied(StatusCode::CONFLICT);
    }
    let guard = match admission.begin(&state) {
        Ok(guard) => guard,
        Err(_) => return denied(StatusCode::CONFLICT),
    };
    let guard = if exec {
        drop(guard);
        None
    } else {
        Some(guard)
    };
    let request = Request::from_parts(parts, Body::from(body));
    let operation = tokio::spawn(async move {
        let _permit = permit;
        if let Some(guard) = guard {
            crate::pro::mutation::reserved_request(guard, next.run(request)).await
        } else {
            next.run(request).await
        }
    });
    // A disconnected/timed-out observer must not cancel a journal rewrite or
    // respawn halfway through and release its reservation. The owned task drains
    // to completion, with the shared 64-operation cap; a stuck filesystem keeps
    // the project fenced rather than permitting a new owner underneath it.
    // Exec already caps shell wait at ten minutes and execution at one hour;
    // preserve that public request budget while still bounding the observer.
    let timeout = Duration::from_secs(if exec { 70 * 60 + 15 } else { 60 });
    match tokio::time::timeout(timeout, operation).await {
        Ok(Ok(response)) => response,
        Ok(Err(_)) => denied(StatusCode::INTERNAL_SERVER_ERROR),
        Err(_) => denied(StatusCode::GATEWAY_TIMEOUT),
    }
}
