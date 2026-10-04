//! Original guard regressions; raw state access stays inside the host.
use super::test_fixture::*;
use super::*;
use axum::http::StatusCode;
use serde_json::json;

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
