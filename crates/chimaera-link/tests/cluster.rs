#![cfg(feature = "fixtures")]
use chimaera_core::{cluster::new_job_id, slurm::LaunchSpec};
use chimaera_link::*;
use futures::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_tungstenite::tungstenite::Message;
struct Fixture {
    keeper: fake::FakeKeeper,
    task: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let keeper = fake::FakeKeeper::new(format!("http://{}", listener.local_addr().unwrap()));
        let router = keeper.router();
        Self {
            keeper,
            task: tokio::spawn(async move { axum::serve(listener, router).await.unwrap() }),
        }
    }
    fn client(&self) -> Client {
        Client::new(&self.keeper.endpoint, Some(fake::FakeKeeper::tokens())).unwrap()
    }
    async fn host(&self) -> Host {
        self.keeper
            .add_cluster_target("cluster", ClusterSnapshot::default())
            .await
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn start() -> ClusterOperation {
    ClusterOperation::StartJob {
        operation_id: format!("op_{}", new_job_id()),
        job_id: new_job_id(),
        name: None,
        spec: LaunchSpec {
            time: "1:00".into(),
            ..Default::default()
        },
        open: vec![],
        startup: String::new(),
        attached: false,
        replaces: None,
        save_as: None,
    }
}
fn jid(op: &ClusterOperation) -> &str {
    match op {
        ClusterOperation::StartJob { job_id, .. } => job_id,
        _ => panic!(),
    }
}
#[tokio::test]
async fn capability_absence_mismatch_and_false_flags_prevent_submission() {
    let f = Fixture::start().await;
    let h = f.host().await;
    let c = f.client();
    let op = start();
    for caps in [
        None,
        Some(ClusterCapabilities {
            version: 2,
            cluster_control_v1: true,
            job_tunnels_v1: true,
        }),
        Some(ClusterCapabilities {
            version: 1,
            cluster_control_v1: false,
            job_tunnels_v1: true,
        }),
        Some(ClusterCapabilities {
            version: 1,
            cluster_control_v1: true,
            job_tunnels_v1: false,
        }),
    ] {
        f.keeper.set_cluster_capabilities(caps).await;
        assert!(c
            .cluster_operation(&h.id, &op)
            .await
            .err()
            .unwrap()
            .is::<ServiceUnsupported>());
        assert!(c.cluster_tcp(&h.id, jid(&op), None).await.is_err());
        assert_eq!(f.keeper.cluster_submissions(&h.id).await, 0);
    }
}
#[tokio::test]
async fn exact_operation_retry_and_history_never_submit_a_second_job() {
    let f = Fixture::start().await;
    let h = f.host().await;
    let c = f.client();
    let op = start();
    // A malformed lost reply follows a committed immutable submission result.
    f.keeper.set_cluster_reply(&h.id,Some(serde_json::json!({"result":"job","job_id":"fixture-private-token","attached":false}))).await;
    let error = c.cluster_operation(&h.id, &op).await.err().unwrap();
    assert!(!format!("{error:?}").contains("fixture-private-token"));
    f.keeper.set_cluster_reply(&h.id, None).await;
    assert!(matches!(
        c.cluster_operation_state(&h.id, &op).await.unwrap(),
        ClusterOperationState::Completed { .. }
    ));
    assert!(matches!(
        c.cluster_operation(&h.id, &op).await.unwrap(),
        ClusterReply::Job { .. }
    ));
    assert_eq!(f.keeper.cluster_submissions(&h.id).await, 1);
    let mut changed = op.clone();
    if let ClusterOperation::StartJob { startup, .. } = &mut changed {
        *startup = "different".into();
    }
    let error = c.cluster_operation(&h.id, &changed).await.err().unwrap();
    assert_eq!(
        error.downcast_ref::<ClusterRequestError>().unwrap().code,
        ClusterErrorCode::OperationChanged
    );
    f.keeper
        .set_cluster_operation_state(
            &h.id,
            op.operation_id().unwrap(),
            ClusterOperationState::Uncertain,
        )
        .await;
    assert!(matches!(
        c.cluster_operation_state(&h.id, &op).await.unwrap(),
        ClusterOperationState::Uncertain
    ));
    assert!(c.cluster_operation(&h.id, &op).await.is_err());
    assert_eq!(f.keeper.cluster_submissions(&h.id).await, 1);
    assert!(c.delete_host(&h.id).await.is_err());
    c.reconnect_host(&h.id).await.unwrap();
    assert_eq!(f.keeper.cluster_submissions(&h.id).await, 1);
}
#[tokio::test]
async fn mismatched_completed_reply_and_unknown_discriminants_fail_closed() {
    let f = Fixture::start().await;
    let h = f.host().await;
    let c = f.client();
    let op = start();
    c.cluster_operation(&h.id, &op).await.unwrap();
    for reply in [
        ClusterReply::Saved,
        ClusterReply::Unknown,
        ClusterReply::Job {
            job_id: new_job_id(),
            slurm_job_id: None,
            attached: false,
        },
    ] {
        f.keeper
            .set_cluster_operation_state(
                &h.id,
                op.operation_id().unwrap(),
                ClusterOperationState::Completed {
                    reply: Box::new(reply),
                },
            )
            .await;
        assert!(c.cluster_operation_state(&h.id, &op).await.is_err());
    }
    let unknown: ClusterOperationState =
        serde_json::from_str("{\"state\":\"future_state\"}").unwrap();
    assert!(matches!(unknown, ClusterOperationState::Unknown));
    assert!(
        serde_json::from_str::<ClusterOperation>("{\"operation\":\"exec\",\"argv\":[\"sh\"]}")
            .is_err()
    );
    assert!(serde_json::from_str::<ClusterOperation>(
        "{\"operation\":\"read_config\",\"argv\":[]}"
    )
    .is_err());
}
#[tokio::test]
async fn bodies_are_bounded_and_unknown_state_cannot_authorize_a_route() {
    let f = Fixture::start().await;
    let h = f.host().await;
    let c = f.client();
    let mut op = start();
    if let ClusterOperation::StartJob { startup, .. } = &mut op {
        *startup = "x".repeat(CLUSTER_TEXT_MAX + 1);
    }
    assert!(c.cluster_operation(&h.id, &op).await.is_err());
    assert_eq!(f.keeper.cluster_submissions(&h.id).await, 0);
    let raw = reqwest::Client::new()
        .post(format!(
            "{}/v1/hosts/{}/cluster/operations",
            f.keeper.endpoint, h.id
        ))
        .bearer_auth(fake::STATIC_TOKEN)
        .header("content-type", "application/json")
        .body("x".repeat(CLUSTER_BODY_MAX + 1))
        .send()
        .await
        .unwrap();
    assert!(!raw.status().is_success());
    let op = start();
    c.cluster_operation(&h.id, &op).await.unwrap();
    let mut snapshot = f.keeper.cluster_snapshot(&h.id).await.unwrap();
    snapshot.jobs[0].state = ClusterJobState::Unknown;
    snapshot.routes.push(ClusterRoute {
        job_id: jid(&op).into(),
        workspace_id: None,
        daemon: Daemon {
            token: "not-logged".into(),
            build: String::new(),
            sessions: 0,
        },
    });
    assert!(ClusterReply::Overview {
        overview: Box::new(snapshot)
    }
    .validate()
    .is_err());
    assert!(c.tcp(&h.id).await.is_err());
    let old:Host=serde_json::from_value(serde_json::json!({"id":"h-old","alias":"ordinary","kind":"ssh","status":"offline","daemon":null,"error":null})).unwrap();
    assert!(old.cluster.is_none());
}
async fn echo(socket: &mut chimaera_link::Socket) {
    let payload = b"cluster-test\0binary\xff";
    socket
        .send(Message::Binary(payload.to_vec().into()))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Binary(data))) => {
                    assert_eq!(data.as_ref(), payload);
                    break;
                }
                Some(Ok(Message::Ping(data))) => socket.send(Message::Pong(data)).await.unwrap(),
                _ => panic!("stream ended"),
            }
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn job_tunnels_survive_device_disconnect_and_stop_only_the_selected_job() {
    let f = Fixture::start().await;
    let h = f.host().await;
    let c = f.client();
    let second = Client::new(
        &f.keeper.endpoint,
        Some(f.keeper.add_device("second").await.unwrap()),
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let echoes = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {accepted=listener.accept()=>{let (mut stream,_)=accepted.unwrap();tasks.spawn(async move {let mut b=[0;4096];while let Ok(n)=stream.read(&mut b).await {if n==0 {break}
            if stream.write_all(&b[..n]).await.is_err(){break}}});},_=tasks.join_next(),if !tasks.is_empty()=>{}}
        }
    });
    let a = start();
    let b = start();
    for op in [&a, &b] {
        c.cluster_operation(&h.id, op).await.unwrap();
        f.keeper
            .set_cluster_target(&h.id, jid(op), None, address)
            .await
            .unwrap();
    }
    let mut first = c.cluster_tcp(&h.id, jid(&a), None).await.unwrap();
    echo(&mut first).await;
    first.close(None).await.unwrap();
    drop(first);
    drop(c);
    let mut a_socket = second.cluster_tcp(&h.id, jid(&a), None).await.unwrap();
    let mut b_socket = second.cluster_tcp(&h.id, jid(&b), None).await.unwrap();
    echo(&mut a_socket).await;
    echo(&mut b_socket).await;
    second
        .cluster_operation(
            &h.id,
            &ClusterOperation::StopJob {
                operation_id: "stop_a".into(),
                job_id: jid(&a).into(),
            },
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(Ok(message)) = a_socket.next().await {
            if matches!(message, Message::Close(_)) {
                break;
            }
        }
    })
    .await
    .unwrap();
    echo(&mut b_socket).await;
    assert!(second.cluster_tcp(&h.id, jid(&a), None).await.is_err());
    assert_eq!(f.keeper.cluster_submissions(&h.id).await, 2);
    echoes.abort();
}
#[tokio::test]
async fn executable_passive_cluster_conformance() {
    let f = Fixture::start().await;
    let h = f.host().await;
    let checks = conformance::run_cluster(&f.client(), &h.id).await.unwrap();
    assert!(checks.len() >= 3);
    let op = ClusterOperation::SetPolicy {
        operation_id: "policy".into(),
        login_serve: true,
        not_cluster: false,
    };
    f.client().cluster_operation(&h.id, &op).await.unwrap();
    let row = f
        .client()
        .hosts()
        .await
        .unwrap()
        .into_iter()
        .find(|row| row.id == h.id)
        .unwrap();
    assert!(row.cluster.unwrap().login_serve);
}

#[tokio::test]
async fn fixture_admission_obeys_global_and_per_host_holder_limits() {
    let f = Fixture::start().await;
    let c = f.client();
    let a = f.host().await;
    let b = f
        .keeper
        .add_cluster_target("second-cluster", ClusterSnapshot::default())
        .await
        .unwrap();
    let d = f
        .keeper
        .add_cluster_target("third-cluster", ClusterSnapshot::default())
        .await
        .unwrap();
    let mut first = None;
    for host in [&a, &b] {
        if host.id == b.id {
            let error = c.cluster_operation(&a.id, &start()).await.err().unwrap();
            assert_eq!(
                error.downcast_ref::<ClusterRequestError>().unwrap().code,
                ClusterErrorCode::JobsHeld
            );
            assert_eq!(f.keeper.cluster_submissions(&b.id).await, 0);
        }
        for _ in 0..8 {
            let op = start();
            c.cluster_operation(&host.id, &op).await.unwrap();
            if first.is_none() {
                first = Some(op);
            }
        }
    }
    for host in [&a, &d] {
        let error = c.cluster_operation(&host.id, &start()).await.err().unwrap();
        assert_eq!(
            error.downcast_ref::<ClusterRequestError>().unwrap().code,
            ClusterErrorCode::JobsHeld
        );
    }
    let first = first.unwrap();
    c.cluster_operation(&a.id, &first).await.unwrap(); // Replay remains legal at capacity.
    c.cluster_operation(
        &a.id,
        &ClusterOperation::StopJob {
            operation_id: "release_holder".into(),
            job_id: jid(&first).into(),
        },
    )
    .await
    .unwrap();
    c.cluster_operation(&d.id, &start()).await.unwrap();
    assert_eq!(f.keeper.cluster_submissions(&a.id).await, 8);
    assert_eq!(f.keeper.cluster_submissions(&b.id).await, 8);
    assert_eq!(f.keeper.cluster_submissions(&d.id).await, 1);
}
#[tokio::test]
async fn fixture_workspace_route_requires_exact_job_association_and_moving_closes_old_route() {
    let f = Fixture::start().await;
    let h = f.host().await;
    let c = f.client();
    let mut ids = vec![];
    for (op, name) in [("add_a", "A"), ("add_b", "B")] {
        let reply = c
            .cluster_operation(
                &h.id,
                &ClusterOperation::AddWorkspace {
                    operation_id: op.into(),
                    path: format!("/home/{name}"),
                    name: name.into(),
                },
            )
            .await
            .unwrap();
        if let ClusterReply::Workspace { workspace } = reply {
            ids.push(workspace.id);
        } else {
            panic!()
        }
    }
    let mut a = start();
    let mut b = start();
    for op in [&mut a, &mut b] {
        if let ClusterOperation::StartJob { open, .. } = op {
            open.push(ids[0].clone());
        }
        c.cluster_operation(&h.id, op).await.unwrap();
    }
    let address = "127.0.0.1:9".parse().unwrap();
    assert!(f
        .keeper
        .set_cluster_target(&h.id, jid(&a), Some(&ids[1]), address)
        .await
        .is_err());
    f.keeper
        .set_cluster_target(&h.id, jid(&a), Some(&ids[0]), address)
        .await
        .unwrap();
    f.keeper
        .set_cluster_target(&h.id, jid(&b), Some(&ids[0]), address)
        .await
        .unwrap();
    assert!(c.cluster_tcp(&h.id, jid(&a), Some(&ids[0])).await.is_err());
    let snapshot = f.keeper.cluster_snapshot(&h.id).await.unwrap();
    assert_eq!(snapshot.routes.len(), 1);
    assert_eq!(snapshot.routes[0].job_id, jid(&b));
    assert_eq!(snapshot.workspaces[0].job.as_deref(), Some(jid(&b)));
    ClusterReply::Overview {
        overview: Box::new(snapshot),
    }
    .validate()
    .unwrap();
}

#[tokio::test]
async fn authenticated_upgrade_cannot_dial_after_revocation_at_its_barrier() {
    let f = Fixture::start().await;
    let h = f.host().await;
    let c = f.client();
    let op = start();
    c.cluster_operation(&h.id, &op).await.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    f.keeper
        .set_cluster_target(&h.id, jid(&op), None, listener.local_addr().unwrap())
        .await
        .unwrap();
    let (entered, release) = f.keeper.pause_cluster_upgrade().await;
    let request = {
        let c = c.clone();
        let host = h.id.clone();
        let job = jid(&op).to_owned();
        tokio::spawn(async move { c.cluster_tcp(&host, &job, None).await })
    };
    tokio::time::timeout(Duration::from_secs(3), entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    c.sign_out_everywhere().await.unwrap();
    release.add_permits(1);
    assert!(request.await.unwrap().is_err());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn positive_batch_refusal_replays_exact_non_submission_without_a_holder() {
    let f = Fixture::start().await;
    let h = f.host().await;
    let c = f.client();
    let op = start();
    f.keeper
        .set_cluster_batch_refusal(&h.id, Some(BatchRefusalKind::BatchNotAllowed))
        .await
        .unwrap();
    assert!(
        matches!(c.cluster_operation(&h.id,&op).await.unwrap(),ClusterReply::Refused {job_id,refusal:BatchRefusalKind::BatchNotAllowed,..} if job_id==jid(&op))
    );
    assert_eq!(f.keeper.cluster_submissions(&h.id).await, 0);
    assert!(f
        .keeper
        .cluster_snapshot(&h.id)
        .await
        .unwrap()
        .records
        .is_empty());
    f.keeper
        .set_cluster_batch_refusal(&h.id, None)
        .await
        .unwrap();
    assert!(matches!(
        c.cluster_operation(&h.id, &op).await.unwrap(),
        ClusterReply::Refused { .. }
    ));
    assert!(
        matches!(c.cluster_operation_state(&h.id,&op).await.unwrap(),ClusterOperationState::Completed {reply} if matches!(reply.as_ref(),ClusterReply::Refused {job_id,..} if job_id==jid(&op)))
    );
    assert_eq!(f.keeper.cluster_submissions(&h.id).await, 0);
    let mut changed = op.clone();
    if let ClusterOperation::StartJob { startup, .. } = &mut changed {
        *startup = "changed".into();
    }
    assert!(c.cluster_operation(&h.id, &changed).await.is_err());
    c.delete_host(&h.id).await.unwrap();
}
#[tokio::test]
async fn refusal_setting_never_reclassifies_an_accepted_uncertain_submission() {
    let f = Fixture::start().await;
    let h = f.host().await;
    let c = f.client();
    let op = start();
    c.cluster_operation(&h.id, &op).await.unwrap();
    f.keeper
        .set_cluster_operation_state(
            &h.id,
            op.operation_id().unwrap(),
            ClusterOperationState::Uncertain,
        )
        .await;
    f.keeper
        .set_cluster_batch_refusal(&h.id, Some(BatchRefusalKind::AccountRequired))
        .await
        .unwrap();
    assert!(c.cluster_operation(&h.id, &op).await.is_err());
    assert!(matches!(
        c.cluster_operation_state(&h.id, &op).await.unwrap(),
        ClusterOperationState::Uncertain
    ));
    assert_eq!(f.keeper.cluster_submissions(&h.id).await, 1);
    assert!(c.delete_host(&h.id).await.is_err());
    assert!(c.cluster_tcp(&h.id, jid(&op), None).await.is_err());
}
#[test]
fn positive_refusal_is_exact_batch_only_and_unknown_classification_is_not_proof() {
    let mut operation = start();
    let reply = ClusterReply::Refused {
        job_id: jid(&operation).into(),
        refusal: BatchRefusalKind::Other,
        slurm_job_id: None,
    };
    assert!(reply.validate().is_ok());
    assert!(operation.accepts(&reply));
    if let ClusterOperation::StartJob { attached, .. } = &mut operation {
        *attached = true;
    }
    assert!(!operation.accepts(&reply));
    if let ClusterOperation::StartJob { attached, .. } = &mut operation {
        *attached = false;
    }
    let wrong = ClusterReply::Refused {
        job_id: new_job_id(),
        refusal: BatchRefusalKind::BatchNotAllowed,
        slurm_job_id: None,
    };
    assert!(!operation.accepts(&wrong));
    let unknown: ClusterReply = serde_json::from_value(
        serde_json::json!({"result":"refused","job_id":jid(&operation),"refusal":"future_reason"}),
    )
    .unwrap();
    assert!(!operation.accepts(&unknown));
    assert!(unknown.validate().is_err());
    assert!(serde_json::from_value::<ClusterReply>(
        serde_json::json!({"result":"refused","job_id":jid(&operation)})
    )
    .is_err());
    assert!(ClusterOperationState::Completed {
        reply: Box::new(unknown)
    }
    .validate_for(&operation)
    .is_err());
}

#[test]
fn refusal_retains_and_rejects_additive_scheduler_identity_in_reply_and_history() {
    let operation = start();
    for id in ["12345", "", "unrecognized"] {
        let reply: ClusterReply = serde_json::from_value(serde_json::json!({
            "result":"refused", "job_id":jid(&operation), "refusal":"other", "slurm_job_id":id
        }))
        .unwrap();
        assert!(!operation.accepts(&reply));
        assert!(reply.validate().is_err());
        let history: ClusterOperationState = serde_json::from_value(serde_json::json!({
            "state":"completed", "reply":reply
        }))
        .unwrap();
        assert!(history.validate_for(&operation).is_err());
    }
}

#[tokio::test]
async fn scheduler_identity_in_refusal_is_refused_over_http_and_history() {
    let f = Fixture::start().await;
    let host = f.host().await;
    let client = f.client();
    let operation = start();
    f.keeper.set_cluster_reply(&host.id, Some(serde_json::json!({
        "result":"refused", "job_id":jid(&operation), "refusal":"other", "slurm_job_id":"12345"
    }))).await;
    assert!(client
        .cluster_operation(&host.id, &operation)
        .await
        .is_err());
    assert_eq!(f.keeper.cluster_submissions(&host.id).await, 1);
    f.keeper
        .set_cluster_operation_state(
            &host.id,
            operation.operation_id().unwrap(),
            ClusterOperationState::Completed {
                reply: Box::new(ClusterReply::Refused {
                    job_id: jid(&operation).into(),
                    refusal: BatchRefusalKind::Other,
                    slurm_job_id: Some("12345".into()),
                }),
            },
        )
        .await;
    assert!(client
        .cluster_operation_state(&host.id, &operation)
        .await
        .is_err());
    assert!(client.delete_host(&host.id).await.is_err());
}
