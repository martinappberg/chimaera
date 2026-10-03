use super::*;
use serde_json::{json, Value};
fn request(command: Value) -> Value {
    json!({"version":1,"binding":{"version":1,"account_id":"a-test","workspace_id":"project-a","project_revision":1,"launch_generation":2,"enrollment":{"version":1,"account_id":"a-test","holder_id":"worker-a","process_boot":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa","registration_generation":1,"worker_credential_digest":"b".repeat(64)}},"request_id":"cccccccc-cccc-cccc-cccc-cccccccccccc","capability":"a".repeat(42)+"A","command":command})
}
fn parsed(command: Value) -> Request {
    Request::decode(&serde_json::to_vec(&request(command)).unwrap()).unwrap()
}
#[test]
fn closed_requests_refuse_unit_variant_extras_duplicate_version_and_unauthoritative_bindings() {
    let good = request(json!({"type":"ready"}));
    assert!(Request::decode(&serde_json::to_vec(&good).unwrap()).is_ok());
    for patch in [
        json!({"type":"ready","path":"/upstream"}),
        json!({"type":"codex_access","refresh_token":"synthetic"}),
        json!({"type":"github_gh_access","argv":[]}),
    ] {
        assert!(Request::decode(&serde_json::to_vec(&request(patch)).unwrap()).is_err());
    }
    for (pointer, value) in [
        ("/version", json!(2)),
        ("/binding/account_id", json!("other")),
        ("/binding/project_revision", json!(0)),
        ("/binding/launch_generation", json!(COUNTER_MAX + 1)),
        (
            "/binding/enrollment/process_boot",
            json!("00000000-0000-0000-0000-000000000000"),
        ),
        ("/request_id", json!("00000000-0000-0000-0000-000000000000")),
    ] {
        let mut bad = good.clone();
        *bad.pointer_mut(pointer).unwrap() = value;
        assert!(Request::decode(&serde_json::to_vec(&bad).unwrap()).is_err());
    }
    let encoded = serde_json::to_string(&good).unwrap();
    let duplicate = encoded.replacen("\"version\":1", "\"version\":1,\"version\":1", 1);
    assert!(Request::decode(duplicate.as_bytes()).is_err());
    assert!(Request::decode(&vec![b' '; CONTROL_MAX + 1]).is_err());
}
#[test]
fn capability_and_access_are_bounded_and_require_canonical_secret_shapes() {
    for bad in [
        String::new(),
        "a".repeat(44),
        "a".repeat(43),
        "a".repeat(42) + "=",
        "a".repeat(42) + "é",
    ] {
        assert!(Capability::new(bad).is_err());
    }
    assert!(Capability::new("a".repeat(42) + "A").is_ok());
    assert!(AccessToken::new("a".repeat(ACCESS_MAX)).is_ok());
    for bad in [
        "a".repeat(ACCESS_MAX + 1),
        "token\n".into(),
        "token space".into(),
        String::new(),
    ] {
        assert!(AccessToken::new(bad).is_err());
    }
}
#[test]
fn github_requests_never_select_a_url_helper_or_non_https_host() {
    assert!(parsed(
        json!({"type":"github_https_credentials","protocol":"https","host":"github.com"})
    )
    .validate()
    .is_ok());
    for command in [
        json!({"type":"github_https_credentials","protocol":"http","host":"github.com"}),
        json!({"type":"github_https_credentials","protocol":"https","host":"github.com:443"}),
        json!({"type":"github_https_credentials","protocol":"https","host":"user@github.com"}),
        json!({"type":"github_https_credentials","protocol":"https","host":"github.com","url":"https://github.com/a"}),
    ] {
        assert!(Request::decode(&serde_json::to_vec(&request(command)).unwrap()).is_err());
    }
}
fn codex_response(r: &Request) -> Value {
    json!({"version":1,"binding":r.binding,"request_id":r.request_id,"result":{"type":"codex_access","access":{"access_token":"synthetic-access","connection_generation":1,"credential_revision":2,"expires_at":2000000000u64,"chatgpt_user_id":"user-a","chatgpt_account_id":"account-a","chatgpt_plan_type":null}}})
}
#[test]
fn codex_reply_matches_original_and_never_accepts_id_or_refresh_tokens() {
    let r = parsed(json!({"type":"codex_refresh","connection_generation":1,"observed_revision":1}));
    let good = codex_response(&r);
    assert!(Response::decode(&serde_json::to_vec(&good).unwrap(), &r).is_ok());
    for (pointer, value) in [
        ("/request_id", json!("dddddddd-dddd-dddd-dddd-dddddddddddd")),
        ("/result/access/credential_revision", json!(1)),
        ("/result/access/connection_generation", json!(2)),
        ("/result/access/expires_at", json!(0)),
        ("/result/access/chatgpt_user_id", json!("")),
        ("/binding/launch_generation", json!(3)),
    ] {
        let mut bad = good.clone();
        *bad.pointer_mut(pointer).unwrap() = value;
        assert!(Response::decode(&serde_json::to_vec(&bad).unwrap(), &r).is_err());
    }
    for name in ["refresh_token", "id_token"] {
        let mut bad = good.clone();
        bad["result"]["access"][name] = json!("synthetic-secret");
        assert!(Response::decode(&serde_json::to_vec(&bad).unwrap(), &r).is_err());
    }
    let mut missing = good.clone();
    missing["result"]["access"]
        .as_object_mut()
        .unwrap()
        .remove("chatgpt_plan_type");
    assert!(Response::decode(&serde_json::to_vec(&missing).unwrap(), &r).is_err());
}
#[test]
fn readiness_and_provider_replies_are_not_interchangeable() {
    let r = parsed(json!({"type":"ready"}));
    assert!(Response::decode(&serde_json::to_vec(&codex_response(&r)).unwrap(), &r).is_err());
    let mut v = json!({"version":1,"binding":r.binding,"request_id":r.request_id,"result":{"type":"ready","ready":true}});
    assert!(Response::decode(&serde_json::to_vec(&v).unwrap(), &r).is_ok());
    v["result"]["ready"] = json!(false);
    assert!(Response::decode(&serde_json::to_vec(&v).unwrap(), &r).is_err());
}
#[test]
fn claude_declared_and_actual_totals_are_checked_without_full_body_buffer() {
    assert!(BodyCount::new(BODY_MAX + 1).is_err());
    let mut count = BodyCount::new(BODY_MAX).unwrap();
    assert!(count.complete().is_err());
    for _ in 0..BODY_MAX / DATA_MAX as u64 {
        count.add(DATA_MAX).unwrap();
    }
    assert_eq!(count.complete(), Ok(BODY_MAX));
    assert!(count.add(1).is_err());
    assert!(count.add(0).is_err());
    let mut short = BodyCount::new(10).unwrap();
    short.add(5).unwrap();
    assert!(short.complete().is_err());
    assert!(Request::decode(
        &serde_json::to_vec(&request(
            json!({"type":"claude_stream","route":"other","content_length":10})
        ))
        .unwrap()
    )
    .is_err());
}
#[test]
fn frames_refuse_unknown_kind_empty_oversize_and_truncated_payload() {
    let raw = FrameHeader::decode(&[1, 0, 1, 0, 0]).unwrap();
    assert_eq!(raw.length, DATA_MAX);
    assert!(raw.check_payload(&[0; 2]).is_err());
    for bad in [
        &[9, 0, 0, 0, 1][..],
        &[1, 0, 1, 0, 1][..],
        &[0, 0, 2, 0, 1][..],
        &[1, 0, 0, 0, 0][..],
        &[0, 1][..],
    ] {
        assert!(FrameHeader::decode(bad).is_err());
    }
    assert!(encode_control(&"x".repeat(CONTROL_MAX)).is_err());
}
#[test]
fn claude_head_and_error_mapping_are_closed_and_fixed() {
    assert_eq!(upstream_status(529), 503);
    assert_eq!(upstream_status(302), 502);
    let r = parsed(json!({"type":"claude_stream","route":"count_tokens","content_length":10}));
    let mut v = json!({"version":1,"binding":r.binding,"request_id":r.request_id,"result":{"type":"claude_head","head":{"status":200,"headers":{"content_type":"application/json","retry_after_seconds":null}}}});
    assert!(Response::decode(&serde_json::to_vec(&v).unwrap(), &r).is_ok());
    v["result"]["head"]["headers"]["content_type"] = json!("text/event-stream");
    assert!(Response::decode(&serde_json::to_vec(&v).unwrap(), &r).is_err());
    v["result"]["head"]["headers"]["content_type"] = json!("application/json");
    v["result"]["head"]["headers"]["set-cookie"] = json!("synthetic");
    assert!(Response::decode(&serde_json::to_vec(&v).unwrap(), &r).is_err());
    for status in [400, 401, 403, 429, 500, 502, 503, 504] {
        let error = ClaudeErrorBody::for_status(status).unwrap();
        assert!(error.validate(status).is_ok());
    }
    let mut error = ClaudeErrorBody::for_status(401).unwrap();
    error.error.message = "raw upstream diagnostic".into();
    assert!(error.validate(401).is_err());
}
