use super::super::maintenance::tests::Fixture;
use super::*;
use chimaera_core::project_secret_idle::{Busy, BusyReason};
use std::os::unix::net::UnixStream as StdStream;

fn pair(f: &Fixture) -> (Channel, UnixStream) {
    let (a, b) = StdStream::pair().unwrap();
    b.set_nonblocking(true).unwrap();
    let channel = Channel::from_inherited(
        &f.state,
        OwnedFd::from(a),
        f.binding.clone(),
        "A".repeat(43),
    )
    .unwrap();
    (channel, UnixStream::from_std(b).unwrap())
}
async fn send(peer: &mut UnixStream, request: &Request) {
    let bytes = request.encode().unwrap();
    peer.write_all(&(bytes.len() as u32).to_be_bytes())
        .await
        .unwrap();
    peer.write_all(&bytes).await.unwrap();
}
fn busy(request: &Request) -> Reply {
    let Request::Prepare(v) = request else {
        panic!("fixture request")
    };
    Reply::Busy(Busy {
        version: v.version,
        request_id: v.request_id,
        binding: v.binding.clone(),
        attempt_id: v.attempt_id.clone(),
        operation_id: v.operation_id.clone(),
        pending_id: v.pending_id.clone(),
        expected_applied_revision: v.expected_applied_revision,
        reason: BusyReason::ProcessUnknown,
    })
}

#[tokio::test]
async fn transferred_unnamed_socket_is_protected_and_reply_correlation_is_exact() {
    let f = Fixture::new().await;
    let (mut channel, mut peer) = pair(&f);
    let flags = unsafe { nix::libc::fcntl(channel.stream.as_raw_fd(), nix::libc::F_GETFD) };
    assert!(flags & nix::libc::FD_CLOEXEC != 0);
    let flags = unsafe { nix::libc::fcntl(channel.stream.as_raw_fd(), nix::libc::F_GETFL) };
    assert!(flags & nix::libc::O_NONBLOCK != 0);
    let request = Request::Prepare(f.prepare());
    send(&mut peer, &request).await;
    let owner = channel.read(&f.state).await.unwrap();
    assert!(owner.request.identity() == request.identity());
    let effect = owner.into_effect(&f.state).await.unwrap();
    assert!(
        effect.owner().request().request_id() == 1 && effect.owner().deadline() > Instant::now()
    );
    channel
        .write(&f.state, effect.owner(), &busy(&request))
        .await
        .unwrap();
    let size = peer.read_u32().await.unwrap() as usize;
    assert!(size <= REPLY_MAX);
    let mut bytes = vec![0; size];
    peer.read_exact(&mut bytes).await.unwrap();
    assert!(
        matches!(Reply::decode(&bytes).unwrap(),Reply::Busy(v) if v.reason==BusyReason::ProcessUnknown)
    );
    let mut wrong = busy(&request);
    if let Reply::Busy(v) = &mut wrong {
        v.pending_id = "55555555-5555-4555-8555-555555555555".into();
    }
    assert!(channel
        .write(&f.state, effect.owner(), &wrong)
        .await
        .is_err());
    assert!(peer.read_u8().await.is_err());
}

#[tokio::test]
async fn named_sockets_and_non_socket_descriptors_never_become_channels() {
    let f = Fixture::new().await;
    let (a, _) = StdStream::pair().unwrap();
    assert!(
        Channel::from_inherited(&f.state, a.into(), f.binding.clone(), "A".repeat(42)).is_err()
    );
    let (datagram, _) = std::os::unix::net::UnixDatagram::pair().unwrap();
    assert!(
        Channel::from_inherited(&f.state, datagram.into(), f.binding.clone(), "A".repeat(43))
            .is_err()
    );
    let file = std::fs::File::open("/dev/null").unwrap();
    assert!(
        Channel::from_inherited(&f.state, file.into(), f.binding.clone(), "A".repeat(43)).is_err()
    );
    let path = std::env::temp_dir().join(format!(
        "chi-idle-{}.sock",
        &chimaera_core::generate_token()[..16]
    ));
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let client = StdStream::connect(&path).unwrap();
    let (server, _) = listener.accept().unwrap();
    assert!(
        Channel::from_inherited(&f.state, client.into(), f.binding.clone(), "A".repeat(43))
            .is_err()
    );
    assert!(
        Channel::from_inherited(&f.state, server.into(), f.binding.clone(), "A".repeat(43))
            .is_err()
    );
    std::fs::remove_file(path).unwrap();
    let (a, b) = StdStream::pair().unwrap();
    let mut wrong = f.binding.clone();
    wrong.registration_revision += 1;
    assert!(Channel::from_inherited(&f.state, a.into(), wrong, "A".repeat(43)).is_err());
    b.set_nonblocking(true).unwrap();
    assert!(UnixStream::from_std(b).unwrap().read_u8().await.is_err());
}

#[tokio::test]
async fn invalid_frames_replays_binding_changes_and_exhaustion_permanently_close() {
    let f = Fixture::new().await;
    for bytes in [
        0u32.to_be_bytes().to_vec(),
        ((REQUEST_MAX + 1) as u32).to_be_bytes().to_vec(),
        vec![0, 0, 0, 2, b'{', b'}'],
    ] {
        let (mut channel, mut peer) = pair(&f);
        peer.write_all(&bytes).await.unwrap();
        assert!(channel.read(&f.state).await.is_err());
        assert!(channel.closed.load(Ordering::Acquire));
    }
    for mode in 0..4 {
        let (mut channel, mut peer) = pair(&f);
        let request = Request::Prepare(f.prepare());
        send(&mut peer, &request).await;
        drop(channel.read(&f.state).await.unwrap());
        let mut changed = f.prepare();
        match mode {
            0 => {}
            1 => changed.binding.account_id = "a-other".into(),
            2 => changed.request_id = u64::MAX,
            _ => {
                f.state.pro.generation.fetch_add(1, Ordering::AcqRel);
            }
        }
        send(&mut peer, &Request::Prepare(changed)).await;
        assert!(channel.read(&f.state).await.is_err());
        assert!(channel.closed.load(Ordering::Acquire));
        if mode == 3 {
            break;
        }
    }
}

#[tokio::test]
async fn cancelled_partial_frame_cannot_reenter_and_partial_frames_have_a_deadline() {
    let f = Fixture::new().await;
    let (mut channel, mut peer) = pair(&f);
    peer.write_u8(0).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), channel.read(&f.state))
            .await
            .is_err()
    );
    assert!(channel.closed.load(Ordering::Acquire));
    assert!(channel.read(&f.state).await.is_err());
    assert!(peer.read_u8().await.is_err());
    let (mut channel, mut peer) = pair(&f);
    peer.write_u8(0).await.unwrap();
    let result = tokio::time::timeout(
        FRAME_DEADLINE + Duration::from_secs(1),
        channel.read(&f.state),
    )
    .await
    .unwrap();
    assert!(result.is_err() && channel.closed.load(Ordering::Acquire));
}

#[tokio::test]
async fn four_owned_requests_bound_queue_and_actual_blocking_owner_keeps_capacity() {
    let f = Fixture::new().await;
    let (mut channel, mut peer) = pair(&f);
    let mut requests = vec![];
    for id in 1..=4 {
        let mut v = f.prepare();
        v.request_id = id;
        send(&mut peer, &Request::Prepare(v)).await;
        requests.push(channel.read(&f.state).await.unwrap());
    }
    assert_eq!(channel.requests.available_permits(), 0);
    assert!(channel.read(&f.state).await.is_err());
    assert!(channel.closed.load(Ordering::Acquire));
    drop(requests);
    assert_eq!(channel.requests.available_permits(), 4);
    let (mut channel, mut peer) = pair(&f);
    send(&mut peer, &Request::Prepare(f.prepare())).await;
    let effect = channel
        .read(&f.state)
        .await
        .unwrap()
        .into_effect(&f.state)
        .await
        .unwrap();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (release, waiting) = std::sync::mpsc::channel();
    let work = tokio::task::spawn_blocking(move || {
        let _owner = effect;
        entered.send(()).unwrap();
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
    });
    ready.await.unwrap();
    work.abort();
    assert_eq!(channel.requests.available_permits(), 3);
    let mut next = f.prepare();
    next.request_id = 2;
    send(&mut peer, &Request::Prepare(next)).await;
    let queued = channel.read(&f.state).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), queued.into_effect(&f.state))
            .await
            .is_err()
    );
    assert_eq!(channel.requests.available_permits(), 3);
    release.send(()).unwrap();
    work.await.unwrap();
    assert_eq!(channel.requests.available_permits(), 4);
}

#[tokio::test]
async fn effect_queue_revalidates_account_generation_after_waiting() {
    let f = Fixture::new().await;
    let (mut channel, mut peer) = pair(&f);
    send(&mut peer, &Request::Prepare(f.prepare())).await;
    let first = channel
        .read(&f.state)
        .await
        .unwrap()
        .into_effect(&f.state)
        .await
        .unwrap();
    let mut second = f.prepare();
    second.request_id = 2;
    send(&mut peer, &Request::Prepare(second)).await;
    let queued = channel.read(&f.state).await.unwrap();
    let future = queued.into_effect(&f.state);
    tokio::pin!(future);
    assert!(tokio::time::timeout(Duration::from_millis(30), &mut future)
        .await
        .is_err());
    f.state.pro.generation.fetch_add(1, Ordering::AcqRel);
    drop(first);
    assert!(future.await.is_err());
    assert_eq!(channel.requests.available_permits(), 4);
}

#[tokio::test]
async fn closed_stream_rejects_queued_effects_before_and_after_waiting() {
    let f = Fixture::new().await;
    let (mut channel, mut peer) = pair(&f);
    send(&mut peer, &Request::Prepare(f.prepare())).await;
    let queued = channel.read(&f.state).await.unwrap();
    peer.write_u32(0).await.unwrap();
    assert!(channel.read(&f.state).await.is_err());
    assert!(queued.into_effect(&f.state).await.is_err());
    assert_eq!(channel.requests.available_permits(), 4);

    let (mut abandoned, mut abandoned_peer) = pair(&f);
    send(&mut abandoned_peer, &Request::Prepare(f.prepare())).await;
    let queued = abandoned.read(&f.state).await.unwrap();
    let capacity = abandoned.requests.clone();
    drop(abandoned);
    assert!(queued.into_effect(&f.state).await.is_err());
    assert_eq!(capacity.available_permits(), 4);

    let (mut channel, mut peer) = pair(&f);
    send(&mut peer, &Request::Prepare(f.prepare())).await;
    let first = channel
        .read(&f.state)
        .await
        .unwrap()
        .into_effect(&f.state)
        .await
        .unwrap();
    let mut next = f.prepare();
    next.request_id = 2;
    send(&mut peer, &Request::Prepare(next)).await;
    let queued = channel.read(&f.state).await.unwrap();
    let future = queued.into_effect(&f.state);
    tokio::pin!(future);
    assert!(tokio::time::timeout(Duration::from_millis(30), &mut future)
        .await
        .is_err());
    peer.write_u8(0).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), channel.read(&f.state))
            .await
            .is_err()
    );
    // Channel loss prevents the waiting admission, but cannot release the
    // already admitted actual owner before its effects/cleanup settle.
    assert_eq!(channel.requests.available_permits(), 2);
    drop(first);
    assert!(future.await.is_err());
    assert_eq!(channel.requests.available_permits(), 4);
}

#[tokio::test]
async fn equal_launch_bindings_do_not_allow_cross_channel_replies() {
    let f = Fixture::new().await;
    let (mut origin, mut origin_peer) = pair(&f);
    let (mut other, mut other_peer) = pair(&f);
    let request = Request::Prepare(f.prepare());
    send(&mut origin_peer, &request).await;
    let owner = origin.read(&f.state).await.unwrap();
    assert!(other
        .write(&f.state, &owner, &busy(&request))
        .await
        .is_err());
    assert!(other.closed.load(Ordering::Acquire));
    assert!(other_peer.read_u8().await.is_err());
    assert!(!origin.closed.load(Ordering::Acquire));
    assert_eq!(origin.requests.available_permits(), 3);
    origin
        .write(&f.state, &owner, &busy(&request))
        .await
        .unwrap();
    let size = origin_peer.read_u32().await.unwrap() as usize;
    let mut bytes = vec![0; size];
    origin_peer.read_exact(&mut bytes).await.unwrap();
    assert!(Reply::decode(&bytes).unwrap().identity() == Some(request.identity()));
    drop(owner);
    assert_eq!(origin.requests.available_permits(), 4);
}
