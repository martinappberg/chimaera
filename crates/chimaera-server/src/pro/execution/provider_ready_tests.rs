//! Real Configure/socket ownership with synthetic protected-launch evidence.
//! This is not a Linux protection or private supervisor integration gate.
use super::*;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chimaera_core::project_secret_idle::{Binding, RootIdentity};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::{
    io::Write,
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::fs::MetadataExt,
    },
    path::PathBuf,
};
use tokio::net::UnixListener;
use tower::ServiceExt;

struct Fixture {
    root: PathBuf,
    state: Arc<AppState>,
    pending: Arc<Pending>,
    listener: UnixListener,
}
impl Fixture {
    fn new(budget: Duration) -> Self {
        // Keep the socket within sockaddr_un on macOS too.
        let root = PathBuf::from("/tmp").join(format!(
            "chimaera-ready-{}-{}",
            std::process::id(),
            &chimaera_core::generate_token()[..16]
        ));
        std::fs::create_dir_all(root.join("project")).unwrap();
        let root = root.canonicalize().unwrap();
        let state = Arc::new(AppState::new(
            "fixture".into(),
            "fixture".into(),
            4242,
            0,
            root.clone(),
            root.join("config"),
        ));
        let meta = std::fs::metadata(root.join("project")).unwrap();
        let launch = Binding {
            account_id: "a-fixture".into(),
            workspace_id: "w-a".into(),
            root_identity: RootIdentity {
                device: meta.dev(),
                inode: meta.ino(),
            },
            registration_revision: 7,
            launch_generation: 1,
            os_boot_id: state.pro.execution.boot.clone().unwrap(),
        };
        let payload = wire::StartupPayload {
            version: 1,
            binding: wire::Binding {
                version: 1,
                account_id: launch.account_id.clone(),
                workspace_id: launch.workspace_id.clone(),
                project_revision: launch.registration_revision,
                launch_generation: launch.launch_generation,
                enrollment: chimaera_core::personal_providers::Registration {
                    version: 1,
                    account_id: launch.account_id.clone(),
                    holder_id: "worker-fixture".into(),
                    process_boot: "00000000-0000-4000-8000-000000000002".into(),
                    registration_generation: 5,
                    worker_credential_digest: "a".repeat(64),
                },
            },
            capability: wire::Capability::new("A".repeat(43)).unwrap(),
        };
        let (reader, writer) = std::io::pipe().unwrap();
        let reader: OwnedFd = reader.into();
        let mut writer = std::fs::File::from(OwnedFd::from(writer));
        writer.write_all(&payload.encode().unwrap()).unwrap();
        drop(writer);
        let descriptor = wire::StartupDescriptor {
            version: 1,
            fd: reader.as_raw_fd(),
        };
        let pending = Arc::new(
            Pending::transferred(
                reader,
                descriptor,
                launch.clone(),
                Instant::now() + Duration::from_secs(1),
                super::super::maintenance_startup::Protection::synthetic(),
            )
            .unwrap(),
        );
        let socket = root.join("ready.sock");
        *lock(&pending.ready.transport) = Some((socket.clone(), budget));
        super::super::supervisor::stage(&state, Some(serde_json::from_value(json!({
            "version":1, "account_id":launch.account_id, "workspace_id":launch.workspace_id,
            "root_identity":launch.root_identity, "registration_revision":launch.registration_revision,
            "launch_generation":launch.launch_generation, "previous_generation":0,
            "os_boot_id":launch.os_boot_id
        })).unwrap()));
        *lock(&state.pro.execution.provider_pending) = Some(pending.clone());
        Self {
            root,
            state,
            pending,
            listener: UnixListener::bind(socket).unwrap(),
        }
    }
    fn config(&self) -> Value {
        json!({"account_id":"a-fixture","role":"worker","endpoint":"http://127.0.0.1:9",
            "keeper_url":"","workspace_root":self.root.join("project"),
            "execution":{"version":1,"installation_id":null,"capability":crate::pro::execution::wire::ExecutionCapability::managed()},
            "delegation":{"access_token":"synthetic","expires_at":"2099-01-01T00:00:00Z",
                "scope":["baton","mirror"],"device_id":"worker-fixture","workspace":{"workspace_id":"w-a","revision":7}}})
    }
    async fn request(&self, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let method = if body.is_some() { "POST" } else { "GET" };
        self.request_method(method, path, body).await
    }
    async fn request_method(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        tokio::time::timeout(Duration::from_secs(3), async {
            let response = crate::app(self.state.clone())
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .header("Authorization", "Bearer fixture")
                        .header("Content-Type", "application/json")
                        .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            (
                status,
                serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            )
        })
        .await
        .unwrap()
    }
    async fn configure(&self) {
        assert_eq!(
            self.request("/api/v1/pro/configure/execution", Some(self.config()))
                .await
                .0,
            StatusCode::OK
        );
    }
    async fn peer(&self) -> (UnixStream, wire::Request) {
        tokio::time::timeout(Duration::from_secs(2), async {
            let (mut socket, _) = self.listener.accept().await.unwrap();
            let mut header = [0; 5];
            socket.read_exact(&mut header).await.unwrap();
            let header = wire::FrameHeader::decode(&header).unwrap();
            assert_eq!(header.kind, wire::FrameKind::RequestBegin);
            let mut bytes = Zeroizing::new(vec![0; wire::CONTROL_MAX]);
            socket
                .read_exact(&mut bytes[..header.length])
                .await
                .unwrap();
            let request = wire::Request::decode(&bytes[..header.length]).unwrap();
            assert!(matches!(request.command, wire::Command::Ready {}));
            assert_eq!(request.capability.expose(), "A".repeat(43));
            (socket, request)
        })
        .await
        .unwrap()
    }
    async fn completed(&self, expected: Phase) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while active(&self.state) != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(lock(&self.pending.ready.inner).phase == expected);
        assert!(!crate::pro::may_execute(&self.state, "w-a"));
        assert!(!crate::pro::may_execute(&self.state, "w-other"));
        assert!(!crate::pro::may_restore(&self.state, "w-a"));
    }
    async fn finish(&self) {
        assert_eq!(
            self.request_method("DELETE", "/api/v1/pro/configure", None)
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(active(&self.state), 0);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        retire(&self.state);
        self.state.stopping.store(true, Ordering::Release);
        for task in [&self.state.pro.task, &self.state.pro.mirror_task] {
            if let Some(task) = lock(task).take() {
                task.abort();
            }
        }
        self.state.changes.notify_waiters();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
async fn frame(socket: &mut UnixStream, kind: u8, body: &[u8]) {
    let mut header = [kind, 0, 0, 0, 0];
    header[1..].copy_from_slice(&(body.len() as u32).to_be_bytes());
    socket.write_all(&header).await.unwrap();
    socket.write_all(body).await.unwrap();
}
async fn reply(socket: &mut UnixStream, request: &wire::Request, refusal: Option<wire::Error>) {
    let (kind, bytes) = if let Some(error) = refusal {
        (
            6,
            wire::encode_control(&wire::Refusal {
                version: 1,
                binding: request.binding.clone(),
                request_id: request.request_id.clone(),
                error,
            })
            .unwrap(),
        )
    } else {
        (
            3,
            wire::encode_control(&wire::Response {
                version: 1,
                binding: request.binding.clone(),
                request_id: request.request_id.clone(),
                result: wire::Reply::Ready { ready: true },
            })
            .unwrap(),
        )
    };
    frame(socket, kind, &bytes).await;
    socket.shutdown().await.unwrap();
}

#[tokio::test]
async fn successful_configure_returns_before_ready_and_only_inactive_retries_the_original() {
    let f = Fixture::new(Duration::from_secs(2));
    // Rejected Configure cannot start the captured capability.
    assert!(f
        .request("/api/v1/pro/configure/execution", Some(json!({})))
        .await
        .0
        .is_client_error());
    assert!(lock(&f.pending.ready.inner).phase == Phase::Staged);
    f.configure().await;
    let (mut first, original) = f.peer().await;
    assert_eq!(active(&f.state), 1);
    assert!(!crate::pro::may_execute(&f.state, "w-a"));
    let deadline = lock(&f.pending.ready.inner).work.as_ref().unwrap().deadline;
    // Ordinary polls and same-identity token refresh neither block on Ready
    // nor replace its request/owner/deadline.
    assert_eq!(f.request("/api/v1/health", None).await.0, StatusCode::OK);
    assert_eq!(
        f.request("/api/v1/pro/status", None).await.0,
        StatusCode::OK
    );
    let mut refreshed = f.config();
    refreshed["delegation"]["access_token"] = json!("synthetic-refreshed");
    assert_eq!(
        f.request("/api/v1/pro/configure/execution", Some(refreshed))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        lock(&f.pending.ready.inner).work.as_ref().unwrap().deadline,
        deadline
    );
    reply(&mut first, &original, Some(wire::Error::Inactive)).await;
    drop(first);
    let (mut second, retry) = f.peer().await;
    assert_eq!(retry.request_id, original.request_id);
    assert!(retry.binding == original.binding);
    assert_eq!(retry.capability.expose(), original.capability.expose());
    reply(&mut second, &retry, None).await;
    drop(second);
    f.completed(Phase::Verified).await;
    f.pending.ready.inspect_fixture(|payload| {
        assert!(payload.binding == original.binding);
        assert_eq!(payload.capability.expose(), original.capability.expose());
    });
    f.configure().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(30), f.listener.accept())
            .await
            .is_err()
    );
    f.finish().await;
}

#[tokio::test]
async fn disconnect_settles_the_actual_socket_before_replacement_can_return() {
    let f = Fixture::new(Duration::from_secs(2));
    f.configure().await;
    let (mut socket, _) = f.peer().await;
    assert_eq!(active(&f.state), 1);
    // The Configure response is already gone; the continuation remains owned.
    // Disconnect holds the real configuration mutex while it awaits closure.
    f.finish().await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), socket.read(&mut [0]))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    f.completed(Phase::Closed).await;
    // No later Configure resurrects this startup, even for the original tuple.
    f.configure().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(30), f.listener.accept())
            .await
            .is_err()
    );
    f.finish().await;
}

#[tokio::test]
async fn stalled_eof_and_unsupported_responses_close_without_poll_or_configure_retry() {
    for refusal in [None, Some(wire::Error::Unsupported)] {
        let f = Fixture::new(Duration::from_millis(300));
        f.configure().await;
        let (mut socket, request) = f.peer().await;
        if let Some(error) = refusal {
            reply(&mut socket, &request, Some(error)).await;
        } else {
            // Complete valid Ready bytes are insufficient without EOF.
            let bytes = wire::encode_control(&wire::Response {
                version: 1,
                binding: request.binding.clone(),
                request_id: request.request_id.clone(),
                result: wire::Reply::Ready { ready: true },
            })
            .unwrap();
            frame(&mut socket, 3, &bytes).await;
        }
        f.completed(Phase::Closed).await;
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), socket.read(&mut [0]))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        f.configure().await;
        assert_eq!(f.request("/api/v1/health", None).await.0, StatusCode::OK);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), f.listener.accept())
                .await
                .is_err()
        );
        f.finish().await;
    }
}

#[tokio::test]
async fn changed_identity_false_ready_and_extra_frame_never_publish_ready() {
    for mismatch in 0..5 {
        let f = Fixture::new(Duration::from_secs(1));
        f.configure().await;
        let (mut socket, request) = f.peer().await;
        let mut response = wire::Response {
            version: 1,
            binding: request.binding.clone(),
            request_id: request.request_id.clone(),
            result: wire::Reply::Ready { ready: true },
        };
        match mismatch {
            0 => response.binding.enrollment.registration_generation += 1,
            1 => response.binding.launch_generation += 1,
            2 => response.request_id = "00000000-0000-4000-8000-000000000001".into(),
            3 => response.result = wire::Reply::Ready { ready: false },
            _ => {}
        }
        frame(&mut socket, 3, &wire::encode_control(&response).unwrap()).await;
        if mismatch == 4 {
            socket.write_all(&[3]).await.unwrap();
        }
        socket.shutdown().await.unwrap();
        f.completed(Phase::Closed).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(30), f.listener.accept())
                .await
                .is_err()
        );
        f.finish().await;
    }
}

#[tokio::test]
async fn retirement_retains_activity_until_io_closure_and_drops_the_capability() {
    let f = Fixture::new(Duration::from_secs(2));
    f.configure().await;
    let (mut socket, _) = f.peer().await;
    // The current-thread executor cannot finish the owned continuation here.
    retire(&f.state);
    assert_eq!(active(&f.state), 1);
    assert!(lock(&f.pending.ready.inner).payload.is_none());
    stop(&f.state).await.unwrap();
    f.completed(Phase::Closed).await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), socket.read(&mut [0]))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    f.finish().await;
}

#[tokio::test]
async fn inactive_retry_keeps_the_first_deadline_and_never_adopts_a_new_launch() {
    let f = Fixture::new(Duration::from_secs(1));
    f.configure().await;
    let (mut socket, original) = f.peer().await;
    let deadline = lock(&f.pending.ready.inner).work.as_ref().unwrap().deadline;
    reply(&mut socket, &original, Some(wire::Error::Inactive)).await;
    drop(socket);
    let (mut socket, retry) = f.peer().await;
    assert_eq!(original.request_id, retry.request_id);
    assert_eq!(
        lock(&f.pending.ready.inner).work.as_ref().unwrap().deadline,
        deadline
    );
    reply(&mut socket, &retry, Some(wire::Error::Inactive)).await;
    // Changing the account generation cannot publish or start a successor.
    f.state.pro.generation.fetch_add(1, Ordering::AcqRel);
    f.completed(Phase::Closed).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(30), f.listener.accept())
            .await
            .is_err()
    );
    f.finish().await;
}

#[tokio::test]
async fn malformed_or_over_limit_frames_close_without_waiting_for_a_body() {
    for oversized in [false, true] {
        let f = Fixture::new(Duration::from_secs(1));
        f.configure().await;
        let (mut socket, _) = f.peer().await;
        if oversized {
            let mut header = [3, 0, 0, 0, 0];
            header[1..].copy_from_slice(&((wire::CONTROL_MAX + 1) as u32).to_be_bytes());
            socket.write_all(&header).await.unwrap();
        } else {
            frame(&mut socket, 3, b"{\"version\":1,\"unexpected\":true}").await;
        }
        // Leave the peer open: schema/size refusal must not wait for EOF or
        // the announced oversized body, and cannot enter Inactive retry.
        f.completed(Phase::Closed).await;
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), socket.read(&mut [0]))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(30), f.listener.accept())
                .await
                .is_err()
        );
        f.finish().await;
    }
}
