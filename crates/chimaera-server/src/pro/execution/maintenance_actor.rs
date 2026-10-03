//! Trusted inherited, one-attempt maintenance actor. A detached actual owner
//! retains admission and cleanup; HTTP/WebSocket callers cannot select it.
use super::{
    maintenance_channel::{Channel, EffectOwner},
    maintenance_park::Parking,
    AppState,
};
use crate::lock;
use chimaera_core::project_secret_idle::*;
use std::{sync::Arc, time::Instant};

struct Attempt {
    identity: AttemptIdentity,
    outcome: Reply,
    parking: Option<Parking>,
    deadline: Instant,
}
fn answer(request: &Request, template: &Reply) -> Reply {
    let mut reply = template.clone();
    // The immutable outcome comes only from the exact retained attempt. A
    // retry changes transport correlation alone, never authority or expiry.
    macro_rules! id {
        ($value:expr) => {
            $value.request_id = request.request_id()
        };
    }
    match &mut reply {
        Reply::Busy(v) => id!(v),
        Reply::Prepared(v) => id!(v),
        Reply::Aborted(v) => id!(v),
        Reply::Expired(v) => id!(v),
        Reply::RecoveryRequired(v) => id!(v),
        Reply::Conflict(v) => id!(v),
        Reply::NotFound(v) => id!(v),
        Reply::Ready(_) => unreachable!(),
    }
    reply
}
macro_rules! response {
    ($request:expr, $kind:ident, $($field:ident: $value:expr),* $(,)?) => {{
        let identity = $request.identity();
        Reply::$kind($kind { version: 1, request_id: $request.request_id(), binding: identity.binding,
            attempt_id: identity.attempt_id, operation_id: identity.operation_id, pending_id: identity.pending_id,
            expected_applied_revision: identity.expected_applied_revision, $($field: $value,)* })
    }};
}
fn terminal(outcome: &Reply) -> bool {
    matches!(
        outcome,
        Reply::Busy(_) | Reply::Aborted(_) | Reply::Expired(_)
    )
}
async fn rollback(attempt: &mut Attempt, request: &Request, expired: bool) {
    let Some(parking) = attempt.parking.take() else {
        return;
    };
    let fence = parking.fence_id.clone();
    attempt.outcome = if parking.rollback().await.is_ok() {
        if expired {
            response!(request, Expired, fence_id: fence)
        } else {
            response!(request, Aborted, fence_id: fence)
        }
    } else {
        response!(request, RecoveryRequired, reason: RecoveryReason::ParkingCleanupUnknown)
    };
}
async fn prepare(state: &Arc<AppState>, effect: &EffectOwner, channel: &Channel) -> Attempt {
    let request = effect.owner().request();
    let Request::Prepare(prepare) = request else {
        unreachable!()
    };
    let deadline = effect.owner().deadline();
    if !channel.process_protected() {
        return Attempt {
            identity: request.identity(),
            outcome: response!(request, Busy, reason: BusyReason::ProcessUnknown),
            parking: None,
            deadline,
        };
    }
    let parking = Parking::admit(state, prepare, deadline).await;
    let mut parking = match parking {
        Ok(parking) => parking,
        Err(reason) => {
            return Attempt {
                identity: request.identity(),
                outcome: response!(request, Busy, reason: reason),
                parking: None,
                deadline,
            }
        }
    };
    let result = parking.prepare().await.and_then(|leaders| {
        if channel.process_protected() {
            Ok(leaders)
        } else {
            Err(BusyReason::ProcessUnknown)
        }
    });
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .map(|v| v.as_millis() as u64)
        .unwrap_or(0);
    let mut attempt = match result {
        Ok(leaders) if remaining > 0 => Attempt {
            identity: request.identity(),
            outcome: response!(request, Prepared, fence_id: parking.fence_id.clone(), remaining_ms: remaining, leaders: leaders),
            parking: Some(parking),
            deadline,
        },
        result => {
            let reason = result.err().unwrap_or(BusyReason::Expired);
            let restored = parking.rollback().await.is_ok();
            Attempt {
                identity: request.identity(),
                outcome: if restored {
                    response!(request, Busy, reason: reason)
                } else {
                    response!(request, RecoveryRequired, reason: RecoveryReason::ParkingCleanupUnknown)
                },
                parking: None,
                deadline,
            }
        }
    };
    if attempt.parking.is_some() && Instant::now() >= deadline {
        rollback(&mut attempt, request, true).await;
    }
    attempt
}
async fn recovery(state: &Arc<AppState>, binding: &Binding) -> anyhow::Result<()> {
    let state = state.clone();
    let binding = binding.clone();
    tokio::task::spawn_blocking(move || {
        let Some(record) = super::maintenance_store::read(&state)? else {
            return Ok(());
        };
        anyhow::ensure!(
            super::supervisor::recovered_park(&state, &record.prepare.binding, &binding),
            "maintenance parking recovery required"
        );
        // Startup already overlaid the retained before-images as manual. Sync
        // that exact current roster before retiring an old launch's receipt.
        let (entries, links) = crate::ledger::snapshot(&state);
        for old in record.sessions()? {
            anyhow::ensure!(
                entries.iter().any(|entry| entry.id == old.id
                    && entry.workspace_id == old.workspace_id
                    && entry.manual_resume_reason.is_some()),
                "maintenance roster recovery required"
            );
        }
        lock(&state.ledger).write_maintenance_durable(&entries, &links)?;
        super::maintenance_store::remove(&state, &record)
    })
    .await?
}
pub(super) async fn run(state: Arc<AppState>, mut channel: Channel) {
    if recovery(&state, channel.binding()).await.is_err() || channel.ready(&state).await.is_err() {
        return;
    }
    let mut retained: Option<Attempt> = None;
    loop {
        let owner = if let Some(attempt) = retained
            .as_ref()
            .filter(|attempt| attempt.parking.is_some())
        {
            tokio::time::timeout_at(
                tokio::time::Instant::from_std(attempt.deadline),
                channel.read(&state),
            )
            .await
            .ok()
            .and_then(Result::ok)
        } else {
            channel.read(&state).await.ok()
        };
        let Some(owner) = owner else {
            if let Some(attempt) = &mut retained {
                // No lost stream can permit a later effect. Cleanup remains in
                // this actual task; its counted owner survives all caller loss.
                let request = Request::Inspect(Inspect {
                    version: 1,
                    request_id: 1,
                    binding: attempt.identity.binding.clone(),
                    attempt_id: attempt.identity.attempt_id.clone(),
                    operation_id: attempt.identity.operation_id.clone(),
                    pending_id: attempt.identity.pending_id.clone(),
                    expected_applied_revision: attempt.identity.expected_applied_revision,
                });
                rollback(attempt, &request, Instant::now() >= attempt.deadline).await;
            }
            return;
        };
        let Ok(effect) = owner.into_effect(&state).await else {
            continue;
        };
        let request = effect.owner().request();
        let identity = request.identity();
        let reply = match request {
            Request::Prepare(_) => {
                match &retained {
                    Some(attempt) if attempt.identity == identity => {}
                    Some(attempt) if !terminal(&attempt.outcome) => {
                        let reply =
                            response!(request, Conflict, reason: ConflictReason::AttemptInProgress);
                        if channel.write(&state, effect.owner(), &reply).await.is_err() {
                            if let Some(attempt) = &mut retained {
                                rollback(attempt, request, Instant::now() >= attempt.deadline)
                                    .await;
                            }
                            break;
                        }
                        continue;
                    }
                    _ => {
                        retained = Some(prepare(&state, &effect, &channel).await);
                    }
                }
                let attempt = retained.as_ref().unwrap();
                let mut reply = answer(request, &attempt.outcome);
                if let Reply::Prepared(value) = &mut reply {
                    value.remaining_ms = attempt
                        .deadline
                        .checked_duration_since(Instant::now())
                        .map(|v| v.as_millis() as u64)
                        .unwrap_or(0);
                }
                reply
            }
            Request::Inspect(_) => match &retained {
                None => response!(request, NotFound,),
                Some(attempt) if attempt.identity != identity => {
                    response!(request, Conflict, reason: ConflictReason::IdentityChanged)
                }
                Some(attempt) => {
                    let mut reply = answer(request, &attempt.outcome);
                    if let Reply::Prepared(value) = &mut reply {
                        value.remaining_ms = attempt
                            .deadline
                            .checked_duration_since(Instant::now())
                            .map(|v| v.as_millis() as u64)
                            .unwrap_or(0);
                    }
                    reply
                }
            },
            Request::Abort(abort) => match &mut retained {
                None => response!(request, NotFound,),
                Some(attempt) if attempt.identity != identity => {
                    response!(request, Conflict, reason: ConflictReason::IdentityChanged)
                }
                Some(attempt) => {
                    let fence = match &attempt.outcome {
                        Reply::Prepared(value) => Some(&value.fence_id),
                        Reply::Aborted(value) => Some(&value.fence_id),
                        Reply::Expired(value) => Some(&value.fence_id),
                        _ => None,
                    };
                    if fence != Some(&abort.fence_id) {
                        response!(request, Conflict, reason: ConflictReason::OutcomeUnknown)
                    } else {
                        rollback(attempt, request, false).await;
                        answer(request, &attempt.outcome)
                    }
                }
            },
        };
        if channel.write(&state, effect.owner(), &reply).await.is_err() {
            if let Some(attempt) = &mut retained {
                rollback(attempt, request, Instant::now() >= attempt.deadline).await;
            }
            break;
        }
    }
    if let Some(attempt) = &mut retained {
        let request = Request::Inspect(Inspect {
            version: 1,
            request_id: 1,
            binding: attempt.identity.binding.clone(),
            attempt_id: attempt.identity.attempt_id.clone(),
            operation_id: attempt.identity.operation_id.clone(),
            pending_id: attempt.identity.pending_id.clone(),
            expected_applied_revision: attempt.identity.expected_applied_revision,
        });
        rollback(attempt, &request, Instant::now() >= attempt.deadline).await;
    }
}
/// Only the opt-in fixed launcher can populate Pending. Ordinary/free startup
/// has no channel, advertisement, timer or process effect.
pub(in crate::pro) fn start(state: &Arc<AppState>) {
    let pending = lock(&state.pro.execution.maintenance_pending).take();
    let Some(pending) = pending else {
        return;
    };
    let state = state.clone();
    tokio::spawn(async move {
        state.wait_restored().await;
        if let Ok(channel) = pending.into_channel(&state) {
            run(state, channel).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::super::maintenance::tests::Fixture;
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    async fn socket(fixture: &Fixture) -> (tokio::task::JoinHandle<()>, tokio::net::UnixStream) {
        fixture
            .state
            .stopping
            .store(false, std::sync::atomic::Ordering::Release);
        let (left, right) = std::os::unix::net::UnixStream::pair().unwrap();
        right.set_nonblocking(true).unwrap();
        let channel = Channel::from_inherited(
            &fixture.state,
            left.into(),
            fixture.binding.clone(),
            "A".repeat(43),
        )
        .unwrap();
        let task = tokio::spawn(run(fixture.state.clone(), channel));
        (task, tokio::net::UnixStream::from_std(right).unwrap())
    }
    async fn send(peer: &mut tokio::net::UnixStream, request: &Request) {
        let bytes = request.encode().unwrap();
        peer.write_all(&(bytes.len() as u32).to_be_bytes())
            .await
            .unwrap();
        peer.write_all(&bytes).await.unwrap();
    }
    async fn reply(peer: &mut tokio::net::UnixStream) -> Reply {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let size = peer.read_u32().await.unwrap() as usize;
            assert!(size <= REPLY_MAX);
            let mut bytes = vec![0; size];
            peer.read_exact(&mut bytes).await.unwrap();
            Reply::decode(&bytes).unwrap()
        })
        .await
        .unwrap()
    }
    fn inspect(prepare: &Prepare, request_id: u64) -> Request {
        Request::Inspect(Inspect {
            version: 1,
            request_id,
            binding: prepare.binding.clone(),
            attempt_id: prepare.attempt_id.clone(),
            operation_id: prepare.operation_id.clone(),
            pending_id: prepare.pending_id.clone(),
            expected_applied_revision: prepare.expected_applied_revision,
        })
    }
    #[tokio::test]
    async fn inherited_ready_empty_park_retry_abort_is_exact_and_never_claims_process_census() {
        let fixture = Fixture::new().await;
        let (task, mut peer) = socket(&fixture).await;
        assert!(matches!(reply(&mut peer).await, Reply::Ready(_)));
        let mut prepare = fixture.prepare();
        prepare.expires_in_ms = 2000;
        send(&mut peer, &inspect(&prepare, 1)).await;
        assert!(matches!(reply(&mut peer).await, Reply::NotFound(_)));
        prepare.request_id = 2;
        send(&mut peer, &Request::Prepare(prepare.clone())).await;
        let Reply::Prepared(first) = reply(&mut peer).await else {
            panic!("positive parking fixture")
        };
        assert!(first.leaders.is_empty()); // Supervisor census is still required.
        assert!(!crate::pro::may_execute(&fixture.state, "w-a"));
        assert!(super::super::maintenance_store::read(&fixture.state)
            .unwrap()
            .is_some());
        prepare.request_id = 3;
        send(&mut peer, &Request::Prepare(prepare.clone())).await;
        let Reply::Prepared(second) = reply(&mut peer).await else {
            panic!("same immutable attempt")
        };
        assert_eq!(first.fence_id, second.fence_id);
        assert!(second.remaining_ms <= first.remaining_ms);
        let abort = Abort {
            version: 1,
            request_id: 4,
            binding: prepare.binding.clone(),
            attempt_id: prepare.attempt_id.clone(),
            operation_id: prepare.operation_id.clone(),
            pending_id: prepare.pending_id.clone(),
            expected_applied_revision: prepare.expected_applied_revision,
            fence_id: first.fence_id.clone(),
        };
        send(&mut peer, &Request::Abort(abort.clone())).await;
        assert!(matches!(reply(&mut peer).await, Reply::Aborted(_)));
        assert!(crate::pro::may_execute(&fixture.state, "w-a"));
        assert!(super::super::maintenance_store::read(&fixture.state)
            .unwrap()
            .is_none());
        let mut duplicate = abort;
        duplicate.request_id = 5;
        send(&mut peer, &Request::Abort(duplicate)).await;
        assert!(matches!(reply(&mut peer).await, Reply::Aborted(_)));
        drop(peer);
        task.await.unwrap();
    }
    #[tokio::test]
    async fn lost_channel_owns_positive_rollback_and_unknown_storage_never_advertises_ready() {
        let fixture = Fixture::new().await;
        let (task, mut peer) = socket(&fixture).await;
        assert!(matches!(reply(&mut peer).await, Reply::Ready(_)));
        let prepare = fixture.prepare();
        send(&mut peer, &Request::Prepare(prepare)).await;
        assert!(matches!(reply(&mut peer).await, Reply::Prepared(_)));
        drop(peer);
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert!(crate::pro::may_execute(&fixture.state, "w-a"));
        std::fs::write(fixture.state.pro.root.join("maintenance-park.json"), b"{").unwrap();
        let (task, mut peer) = socket(&fixture).await;
        task.await.unwrap();
        assert!(peer.read_u8().await.is_err());
    }
    #[tokio::test]
    async fn original_deadline_expires_without_any_inspect_or_timeout_extension() {
        let fixture = Fixture::new().await;
        let (task, mut peer) = socket(&fixture).await;
        assert!(matches!(reply(&mut peer).await, Reply::Ready(_)));
        let mut prepare = fixture.prepare();
        prepare.expires_in_ms = 100;
        send(&mut peer, &Request::Prepare(prepare)).await;
        assert!(matches!(reply(&mut peer).await, Reply::Prepared(_)));
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert!(crate::pro::may_execute(&fixture.state, "w-a"));
        assert!(peer.read_u8().await.is_err());
        assert!(super::super::maintenance_store::read(&fixture.state)
            .unwrap()
            .is_none());
    }
}
