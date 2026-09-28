//! A failed startup probe retains its rotated candidate without granting access.
use super::lock;
use std::sync::{Arc, Mutex};

pub(super) const READ_WARNING: &str = "account_restore_locked";
pub(super) const NETWORK_WARNING: &str = "account_restore_unavailable";
pub(super) const RETRIES: &[u64] = &[2, 5, 15];

pub(super) struct Recovery<T> {
    inner: Arc<Mutex<Inner<T>>>,
}
struct Inner<T> {
    generation: u64,
    revision: u64,
    running: bool,
    candidate: Option<T>,
}
impl<T> Default for Recovery<T> {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                generation: 0,
                revision: 0,
                running: false,
                candidate: None,
            })),
        }
    }
}
impl<T: Clone> Recovery<T> {
    pub(super) fn begin(&self, generation: u64) -> Option<Attempt<T>> {
        let mut inner = lock(&self.inner);
        if inner.generation != generation {
            inner.generation = generation;
            inner.revision = inner.revision.wrapping_add(1);
            inner.candidate = None;
            inner.running = false;
        }
        if inner.running {
            return None;
        }
        inner.running = true;
        Some(Attempt {
            inner: self.inner.clone(),
            generation,
            revision: inner.revision,
        })
    }
    pub(super) fn supersede(&self) {
        let mut inner = lock(&self.inner);
        inner.revision = inner.revision.wrapping_add(1);
        inner.running = false;
    }
    pub(super) fn has_candidate(&self, generation: u64) -> bool {
        let inner = lock(&self.inner);
        inner.generation == generation && inner.candidate.is_some()
    }
    pub(super) fn cancel(&self) {
        let mut inner = lock(&self.inner);
        inner.revision = inner.revision.wrapping_add(1);
        inner.candidate = None;
        inner.running = false;
    }
}
pub(super) struct Attempt<T> {
    inner: Arc<Mutex<Inner<T>>>,
    generation: u64,
    revision: u64,
}
impl<T: Clone> Attempt<T> {
    pub(super) fn current(&self, generation: u64) -> bool {
        let inner = lock(&self.inner);
        generation == self.generation
            && inner.generation == self.generation
            && inner.revision == self.revision
    }
    pub(super) fn candidate(&self) -> Option<T> {
        let inner = lock(&self.inner);
        (inner.generation == self.generation && inner.revision == self.revision)
            .then(|| inner.candidate.clone())
            .flatten()
    }
    pub(super) fn remember(&self, candidate: T) {
        let mut inner = lock(&self.inner);
        if inner.generation == self.generation && inner.revision == self.revision {
            inner.candidate = Some(candidate);
        }
    }
}
impl<T> Drop for Attempt<T> {
    fn drop(&mut self) {
        let mut inner = lock(&self.inner);
        if inner.generation == self.generation && inner.revision == self.revision {
            inner.running = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retry_preserves_the_same_rotating_candidate() {
        let recovery = Recovery::default();
        let attempt = recovery.begin(7).unwrap();
        let tokens = Arc::new(Mutex::new("old"));
        attempt.remember(tokens.clone());
        *lock(&tokens) = "rotated";
        assert!(recovery.begin(7).is_none(), "only one attempt may probe");
        drop(attempt);
        let retry = recovery.begin(7).unwrap();
        assert_eq!(*lock(&retry.candidate().unwrap()), "rotated");
    }
    #[test]
    fn replacement_and_signout_fence_old_results_and_cleanup() {
        let recovery = Recovery::default();
        let old = recovery.begin(7).unwrap();
        old.remember("old");
        let next = recovery.begin(8).unwrap();
        assert!(next.candidate().is_none());
        old.remember("late old rotation");
        drop(old);
        assert!(
            recovery.begin(8).is_none(),
            "old cleanup must not unlock newer probe"
        );
        assert!(next.candidate().is_none());
        recovery.cancel();
        assert!(!next.current(8));
        next.remember("resurrection");
        drop(next);
        assert!(recovery.begin(8).unwrap().candidate().is_none());
    }
    #[test]
    fn canceled_read_does_not_install_a_candidate() {
        let recovery = Recovery::default();
        let old = recovery.begin(1).unwrap();
        recovery.cancel();
        let new = recovery.begin(1).unwrap();
        old.remember("stale read");
        assert!(!old.current(1));
        assert!(new.current(1));
        assert!(new.candidate().is_none());
    }
    #[test]
    fn explicit_signin_fences_restore_but_retains_rotation_for_canceled_login() {
        let recovery = Recovery::default();
        let old = recovery.begin(1).unwrap();
        let tokens = Arc::new(Mutex::new("before"));
        old.remember(tokens.clone());
        recovery.supersede();
        *lock(&tokens) = "after";
        assert!(
            !old.current(1),
            "late restore must not install over explicit sign-in"
        );
        assert!(recovery.has_candidate(1));
        let retry = recovery.begin(1).unwrap();
        assert_eq!(*lock(&retry.candidate().unwrap()), "after");
        drop(old);
        assert!(recovery.begin(1).is_none());
    }

    #[tokio::test]
    async fn failed_account_read_after_rotation_retries_the_retained_real_client() {
        use chimaera_link::{Client, Tokens};
        use serde_json::json;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            for step in 0..4 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut chunk = [0u8; 2048];
                    let n = stream.read(&mut chunk).await.unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&chunk[..n]);
                    assert!(request.len() < 16384);
                    if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                        let length = header
                            .lines()
                            .find_map(|line| {
                                line.strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                let request = String::from_utf8(request).unwrap();
                let (status, body) = match step {
                    0 => {
                        assert!(request.starts_with("GET /v1/me "));
                        assert!(request.contains("before-access"));
                        ("401 Unauthorized", json!({"error":"expired"}))
                    }
                    1 => {
                        assert!(request.starts_with("POST /v1/oauth/refresh "));
                        assert!(request.contains("before-refresh"));
                        (
                            "200 OK",
                            json!({"access_token":"after-access","refresh_token":"after-refresh","token_type":"Bearer","expires_in":3600}),
                        )
                    }
                    2 => {
                        assert!(request.starts_with("GET /v1/me "));
                        assert!(request.contains("after-access"));
                        (
                            "503 Service Unavailable",
                            json!({"error":"temporarily_unavailable"}),
                        )
                    }
                    _ => {
                        assert!(request.starts_with("GET /v1/me "));
                        assert!(request.contains("after-access"));
                        (
                            "200 OK",
                            json!({"account_id":"fixture","email":"fixture@example.invalid","device_id":"d-fixture","plan":"none","protocol":0,"keeper_url":"","limits":{"cloud_hours":0,"storage_bytes":0},"usage":{"cloud_hours":0,"storage_bytes":0},"hours_exhausted":false}),
                        )
                    }
                };
                let body = body.to_string();
                stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                stream.shutdown().await.unwrap();
            }
        });
        let client = Client::new(
            &endpoint,
            Some(Tokens {
                access_token: "before-access".into(),
                refresh_token: "before-refresh".into(),
                token_type: "Bearer".into(),
                expires_in: 1,
            }),
        )
        .unwrap();
        let recovery = Recovery::default();
        let attempt = recovery.begin(1).unwrap();
        attempt.remember(client.clone());
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            assert!(super::super::account_snapshot(&client).await.is_err());
            let saved = client.tokens().await.unwrap();
            assert_eq!(saved.refresh_token, "after-refresh");
            drop(attempt);
            let retry = recovery.begin(1).unwrap();
            let retained = retry.candidate().unwrap();
            let (account, _, _) = super::super::account_snapshot(&retained).await.unwrap();
            assert_eq!(account.account_id, "fixture");
            assert_eq!(
                retained.tokens().await.unwrap().refresh_token,
                "after-refresh"
            );
            server.await.unwrap();
        })
        .await
        .unwrap();
    }
}
