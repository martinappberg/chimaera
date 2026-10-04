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

pub struct Fixture {
    pub root: PathBuf,
    pub(in crate::pro::execution) state: Arc<AppState>,
    pub(in crate::pro::execution) pending: Arc<Pending>,
    pub listener: UnixListener,
}
impl Fixture {
    #[cfg(feature = "daemon-extension-fixture")]
    pub fn context(&self) -> super::super::provider_fixture_host::Context {
        super::super::provider_fixture_host::Context::from_state(self.state.clone()).unwrap()
    }
    pub fn active(&self) -> usize {
        active(&self.state)
    }

    pub fn new(budget: Duration) -> Self {
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
                super::super::provider_protection::Protection::synthetic(),
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
    pub async fn verified(&self) {
        self.configure().await;
        let (mut socket, request) = self.peer().await;
        let reply = wire::Response {
            version: 1,
            binding: request.binding.clone(),
            request_id: request.request_id.clone(),
            result: wire::Reply::Ready { ready: true },
        };
        frame(&mut socket, 3, &wire::encode_control(&reply).unwrap()).await;
        socket.shutdown().await.unwrap();
        drop(socket);
        self.completed(Phase::Verified).await;
    }
    pub(super) fn config(&self) -> Value {
        json!({"account_id":"a-fixture","role":"worker","endpoint":"http://127.0.0.1:9",
            "keeper_url":"","workspace_root":self.root.join("project"),
            "execution":{"version":1,"installation_id":null,"capability":crate::pro::execution::wire::ExecutionCapability::managed()},
            "delegation":{"access_token":"synthetic","expires_at":"2099-01-01T00:00:00Z",
                "scope":["baton","mirror"],"device_id":"worker-fixture","workspace":{"workspace_id":"w-a","revision":7}}})
    }
    pub(super) async fn request(&self, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let method = if body.is_some() { "POST" } else { "GET" };
        self.request_method(method, path, body).await
    }
    pub(super) async fn request_method(
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
    pub(super) async fn configure(&self) {
        assert_eq!(
            self.request("/api/v1/pro/configure/execution", Some(self.config()))
                .await
                .0,
            StatusCode::OK
        );
    }
    pub(super) async fn peer(&self) -> (UnixStream, wire::Request) {
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
    pub(super) async fn completed(&self, expected: Phase) {
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
    pub async fn finish(&self) {
        assert_eq!(
            self.request_method("DELETE", "/api/v1/pro/configure", None)
                .await
                .0,
            StatusCode::NO_CONTENT
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
pub(super) async fn frame(socket: &mut UnixStream, kind: u8, body: &[u8]) {
    let mut header = [kind, 0, 0, 0, 0];
    header[1..].copy_from_slice(&(body.len() as u32).to_be_bytes());
    socket.write_all(&header).await.unwrap();
    socket.write_all(body).await.unwrap();
}
#[cfg(test)]
pub(super) async fn reply(
    socket: &mut UnixStream,
    request: &wire::Request,
    refusal: Option<wire::Error>,
) {
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
