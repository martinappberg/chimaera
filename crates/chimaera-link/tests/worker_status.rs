use chimaera_link::{WorkerPhase, WorkerReason, WorkerState, WorkerStatus};
use serde::Deserialize;
use serde_json::json;

#[test]
fn worker_status_accepts_older_services_and_omits_absent_phase() {
    for state in [
        "no_plan",
        "unavailable",
        "preparing",
        "ready",
        "sleeping",
        "limited",
        "error",
    ] {
        let body = json!({"state": state, "reason": null});
        let status: WorkerStatus = serde_json::from_value(body.clone()).unwrap();
        assert_eq!(status.phase, None);
        assert_eq!(serde_json::to_value(status).unwrap(), body);
    }
    let status: WorkerStatus = serde_json::from_value(json!({
        "state": "unavailable", "reason": "provisioning_disabled", "phase": null
    }))
    .unwrap();
    assert_eq!(status.reason, Some(WorkerReason::ProvisioningDisabled));
    assert_eq!(status.phase, None);
}

#[test]
fn confirmed_phases_round_trip_without_changing_legacy_status() {
    #[derive(Deserialize)]
    struct LegacyStatus {
        state: WorkerState,
        reason: Option<WorkerReason>,
    }
    for (wire, phase) in [
        ("keeper", WorkerPhase::Keeper),
        ("worker", WorkerPhase::Worker),
        ("connecting", WorkerPhase::Connecting),
    ] {
        let body = json!({"state": "preparing", "reason": null, "phase": wire});
        let status: WorkerStatus = serde_json::from_value(body.clone()).unwrap();
        assert_eq!(status.state, WorkerState::Preparing);
        assert_eq!(status.phase, Some(phase));
        // Native IPC serializes this same type; no phase mapping is needed.
        assert_eq!(serde_json::to_value(status).unwrap(), body);
        let old_client: LegacyStatus = serde_json::from_value(body).unwrap();
        assert_eq!(old_client.state, WorkerState::Preparing);
        assert_eq!(old_client.reason, None);
    }
}
