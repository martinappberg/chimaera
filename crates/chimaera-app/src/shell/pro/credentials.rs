//! A failed credential-store write must not revoke a valid in-memory session.
//! One writer coalesces rotations; sign-out/replacement fences its queued writes.
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

use anyhow::Result;
use chimaera_link::Tokens;
use tokio::sync::{watch, Notify};

use super::lock;

pub(super) const SAVE_WARNING: &str = "account_credentials_unsaved";
const BACKOFF: &[Duration] = &[
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(60),
];

#[derive(Default)]
pub(super) struct Persistence {
    failed_generation: Mutex<Option<u64>>,
    retry: Notify,
}
impl Persistence {
    pub(super) fn warning(&self, generation: u64) -> Option<&'static str> {
        (*lock(&self.failed_generation) == Some(generation)).then_some(SAVE_WARNING)
    }
    pub(super) fn retry(&self) {
        self.retry.notify_one();
    }
    fn recovered(&self, generation: u64) {
        let mut failed = lock(&self.failed_generation);
        if *failed == Some(generation) {
            *failed = None;
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum End {
    Revoked,
    Superseded,
    Closed,
}

type Write = Arc<dyn Fn(&Tokens) -> Result<()> + Send + Sync>;

struct Saved {
    tokens: Option<Tokens>,
    success: bool,
}

fn same_pair(a: &Option<Tokens>, b: &Option<Tokens>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => {
            a.access_token == b.access_token && a.refresh_token == b.refresh_token
        }
        (None, None) => true,
        _ => false,
    }
}

pub(super) struct Writer {
    pub updates: watch::Receiver<Option<Tokens>>,
    pub generation: Arc<AtomicU64>,
    pub expected: u64,
    pub serialization: Arc<Mutex<()>>,
    pub persistence: Arc<Persistence>,
}
impl Writer {
    pub(super) async fn run(
        self,
        dirty: bool,
        write: impl Fn(&Tokens) -> Result<()> + Send + Sync + 'static,
        changed: impl Fn() + Send + Sync,
    ) -> End {
        self.run_with_backoff(dirty, Arc::new(write), changed, BACKOFF)
            .await
    }

    async fn run_with_backoff(
        mut self,
        mut dirty: bool,
        write: Write,
        changed: impl Fn() + Send + Sync,
        backoff: &[Duration],
    ) -> End {
        let mut failures = 0;
        let mut retry_at = None;
        loop {
            if self.generation.load(Ordering::SeqCst) != self.expected {
                return End::Superseded;
            }
            if self.updates.borrow().is_none() {
                return End::Revoked;
            }
            if !dirty {
                tokio::select! {
                    biased;
                    result = self.updates.changed() => {
                        if result.is_err() { return End::Closed; }
                        failures = 0;
                    }
                    _ = self.persistence.retry.notified() => { failures = 0; }
                    _ = async {
                        match retry_at {
                            Some(at) => tokio::time::sleep_until(at).await,
                            None => std::future::pending().await,
                        }
                    } => {}
                }
            }
            dirty = false;
            retry_at = None;
            if self.generation.load(Ordering::SeqCst) != self.expected {
                return End::Superseded;
            }
            if self.updates.borrow_and_update().is_none() {
                return End::Revoked;
            }
            let updates = self.updates.clone();
            let generation = self.generation.clone();
            let expected = self.expected;
            let serialization = self.serialization.clone();
            let write = write.clone();
            let mut saving = tokio::task::spawn_blocking(move || {
                let _serialized = lock(&serialization);
                if generation.load(Ordering::SeqCst) != expected {
                    return None;
                }
                // Snapshot only after acquiring the shared I/O lock. Do not hold
                // the watch/token lock across an OS call: rotation must stay live.
                let tokens = updates.borrow().clone();
                let success = tokens.as_ref().is_some_and(|tokens| write(tokens).is_ok());
                Some(Saved { tokens, success })
            });
            let saved = loop {
                tokio::select! {
                    biased;
                    result = self.updates.changed() => {
                        if result.is_err() { return End::Closed; }
                        if self.generation.load(Ordering::SeqCst) != self.expected { return End::Superseded; }
                        // Revocation never waits for a slow OS write. The caller's
                        // serialized deletion follows any already-running write.
                        if self.updates.borrow().is_none() { return End::Revoked; }
                        failures = 0;
                    }
                    result = &mut saving => { break result; }
                }
            };
            if self.generation.load(Ordering::SeqCst) != self.expected {
                return End::Superseded;
            }
            if self.updates.borrow().is_none() {
                return End::Revoked;
            }
            let saved = match saved {
                Ok(Some(saved)) => saved,
                Ok(None) => return End::Superseded,
                Err(_) => Saved {
                    tokens: self.updates.borrow().clone(),
                    success: false,
                },
            };
            if !same_pair(&saved.tokens, &self.updates.borrow()) {
                dirty = true;
                failures = 0;
                continue;
            }
            if saved.success {
                self.persistence.recovered(self.expected);
                failures = 0;
                changed();
            } else if let Some(delay) = backoff.get(failures) {
                failures += 1;
                retry_at = Some(tokio::time::Instant::now() + *delay);
            } else {
                *lock(&self.persistence.failed_generation) = Some(self.expected);
                changed();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use tokio::sync::mpsc;

    const SHORT_RETRY: &[Duration] = &[Duration::from_millis(10)];
    const LONG_RETRY: &[Duration] = &[Duration::from_secs(60)];

    fn tokens(name: &str) -> Tokens {
        Tokens {
            access_token: name.into(),
            refresh_token: format!("refresh-{name}"),
            token_type: "Bearer".into(),
            expires_in: 3600,
        }
    }
    struct Fixture {
        updates: watch::Sender<Option<Tokens>>,
        generation: Arc<AtomicU64>,
        persistence: Arc<Persistence>,
        serialization: Arc<Mutex<()>>,
    }
    impl Fixture {
        fn new() -> Self {
            Self {
                updates: watch::channel(Some(tokens("first"))).0,
                generation: Arc::new(AtomicU64::new(1)),
                persistence: Arc::new(Persistence::default()),
                serialization: Arc::new(Mutex::new(())),
            }
        }
        fn writer(&self) -> Writer {
            Writer {
                updates: self.updates.subscribe(),
                generation: self.generation.clone(),
                expected: 1,
                serialization: self.serialization.clone(),
                persistence: self.persistence.clone(),
            }
        }
        async fn stop(&self, task: tokio::task::JoinHandle<End>) {
            self.updates.send_replace(None);
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), task)
                    .await
                    .unwrap()
                    .unwrap(),
                End::Revoked
            );
        }
    }
    async fn next<T>(rx: &mut mpsc::UnboundedReceiver<T>) -> T {
        tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn temporary_store_failure_retains_tokens_and_recovers_without_a_warning() {
        let fixture = Fixture::new();
        let (attempt, mut attempts) = mpsc::unbounded_channel();
        let (changed, mut changes) = mpsc::unbounded_channel();
        let first = AtomicBool::new(true);
        let task = tokio::spawn(fixture.writer().run_with_backoff(
            true,
            Arc::new(move |tokens| {
                attempt.send(tokens.access_token.clone()).unwrap();
                anyhow::ensure!(
                    !first.swap(false, Ordering::SeqCst),
                    "temporary store failure"
                );
                Ok(())
            }),
            move || {
                let _ = changed.send(());
            },
            SHORT_RETRY,
        ));
        assert_eq!(next(&mut attempts).await, "first");
        assert!(fixture.updates.borrow().is_some());
        assert!(fixture.persistence.warning(1).is_none());
        assert_eq!(next(&mut attempts).await, "first");
        next(&mut changes).await;
        assert!(fixture.persistence.warning(1).is_none());
        fixture.stop(task).await;
    }

    #[tokio::test]
    async fn exhausted_retries_wait_for_explicit_retry_or_new_tokens() {
        let fixture = Fixture::new();
        let (attempt, mut attempts) = mpsc::unbounded_channel();
        let (changed, mut changes) = mpsc::unbounded_channel();
        let fail = Arc::new(AtomicBool::new(true));
        let store_fail = fail.clone();
        let task = tokio::spawn(fixture.writer().run_with_backoff(
            true,
            Arc::new(move |tokens| {
                attempt.send(tokens.access_token.clone()).unwrap();
                anyhow::ensure!(!store_fail.load(Ordering::SeqCst), "store unavailable");
                Ok(())
            }),
            move || {
                let _ = changed.send(());
            },
            SHORT_RETRY,
        ));
        next(&mut attempts).await;
        next(&mut attempts).await;
        next(&mut changes).await;
        assert_eq!(fixture.persistence.warning(1), Some(SAVE_WARNING));
        assert!(fixture.persistence.warning(2).is_none());
        assert!(
            tokio::time::timeout(Duration::from_millis(30), attempts.recv())
                .await
                .is_err()
        );
        assert!(fixture.updates.borrow().is_some());
        fail.store(false, Ordering::SeqCst);
        fixture.persistence.retry();
        next(&mut attempts).await;
        next(&mut changes).await;
        assert!(fixture.persistence.warning(1).is_none());
        fixture.stop(task).await;
    }

    #[tokio::test]
    async fn rotation_interrupts_retry_delay_and_persists_the_latest_pair() {
        let fixture = Fixture::new();
        let (attempt, mut attempts) = mpsc::unbounded_channel();
        let task = tokio::spawn(fixture.writer().run_with_backoff(
            true,
            Arc::new(move |tokens| {
                attempt.send(tokens.access_token.clone()).unwrap();
                anyhow::ensure!(tokens.access_token != "first", "temporary store failure");
                Ok(())
            }),
            || {},
            LONG_RETRY,
        ));
        assert_eq!(next(&mut attempts).await, "first");
        fixture.updates.send_replace(Some(tokens("rotated")));
        assert_eq!(next(&mut attempts).await, "rotated");
        fixture.stop(task).await;
    }

    #[tokio::test]
    async fn stale_success_cannot_clear_warning_before_the_latest_rotation_is_saved() {
        let fixture = Fixture::new();
        *lock(&fixture.persistence.failed_generation) = Some(1);
        let (attempt, mut attempts) = mpsc::unbounded_channel();
        let (changed, mut changes) = mpsc::unbounded_channel();
        let (release, released) = std::sync::mpsc::channel();
        let released = Mutex::new(released);
        let task = tokio::spawn(fixture.writer().run_with_backoff(
            true,
            Arc::new(move |tokens| {
                attempt.send(tokens.access_token.clone()).unwrap();
                lock(&released)
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap();
                Ok(())
            }),
            move || {
                let _ = changed.send(());
            },
            &[],
        ));
        assert_eq!(next(&mut attempts).await, "first");
        fixture.updates.send_replace(Some(tokens("superseded")));
        fixture.updates.send_replace(Some(tokens("latest")));
        release.send(()).unwrap();
        assert_eq!(next(&mut attempts).await, "latest");
        assert_eq!(fixture.persistence.warning(1), Some(SAVE_WARNING));
        assert!(changes.try_recv().is_err());
        release.send(()).unwrap();
        next(&mut changes).await;
        assert!(fixture.persistence.warning(1).is_none());
        fixture.stop(task).await;
    }

    #[tokio::test]
    async fn sign_out_observes_revocation_while_store_is_blocked_and_deletes_last() {
        let fixture = Fixture::new();
        let saved = Arc::new(Mutex::new(Vec::<String>::new()));
        let written = saved.clone();
        let (entered, mut entries) = mpsc::unbounded_channel();
        let (release, released) = std::sync::mpsc::channel();
        let released = Mutex::new(released);
        let task = tokio::spawn(fixture.writer().run_with_backoff(
            true,
            Arc::new(move |tokens| {
                entered.send(()).unwrap();
                lock(&released)
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap();
                lock(&written).push(tokens.access_token.clone());
                Ok(())
            }),
            || {},
            &[],
        ));
        next(&mut entries).await;
        fixture.updates.send_replace(None);
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(100), task)
                .await
                .unwrap()
                .unwrap(),
            End::Revoked
        );
        fixture.generation.fetch_add(1, Ordering::SeqCst);
        let serialization = fixture.serialization.clone();
        let deleted = saved.clone();
        let deletion = tokio::task::spawn_blocking(move || {
            let _serialized = lock(&serialization);
            lock(&deleted).push("deleted".into());
        });
        release.send(()).unwrap();
        deletion.await.unwrap();
        assert_eq!(*lock(&saved), ["first", "deleted"]);
        assert!(fixture.persistence.warning(2).is_none());
    }

    #[tokio::test]
    async fn replacement_fences_queued_old_account_writes_before_store_access() {
        let fixture = Fixture::new();
        let serialization = fixture.serialization.clone();
        let (entered, mut entries) = mpsc::unbounded_channel();
        let (release, released) = std::sync::mpsc::channel();
        let holder = tokio::task::spawn_blocking(move || {
            let _held = lock(&serialization);
            entered.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(2)).unwrap();
        });
        next(&mut entries).await;
        let touched = Arc::new(AtomicBool::new(false));
        let store_touched = touched.clone();
        let task = tokio::spawn(fixture.writer().run_with_backoff(
            true,
            Arc::new(move |_| {
                store_touched.store(true, Ordering::SeqCst);
                Ok(())
            }),
            || {},
            &[],
        ));
        tokio::task::yield_now().await;
        fixture.generation.store(2, Ordering::SeqCst);
        release.send(()).unwrap();
        holder.await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap(),
            End::Superseded
        );
        assert!(!touched.load(Ordering::SeqCst));
        assert!(fixture.persistence.warning(2).is_none());
    }
}
