use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use signature::Signer;
use ssh_key::private::{Ed25519Keypair, KeypairData};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("cr-{}", &chimaera_core::generate_token()[..16]));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn jump_arguments_are_explicit_and_cannot_smuggle_a_command_or_uri_options() {
    for value in [
        "visitor@jump:2222",
        "ssh://visitor@jump:2222",
        "visitor@[::1]:2222",
    ] {
        let jump = Jump::parse(value).unwrap();
        assert_eq!(jump.user.as_deref(), Some("visitor"));
        assert_eq!(jump.port, Some(2222));
    }
    for value in [
        "-F/config",
        "host;touch",
        "host a",
        "ssh://user:password@host",
        "ssh://host/a",
        "ssh://host?x",
        "a:b:c",
        "user@@host",
        "host:0",
        "host:65536",
    ] {
        assert!(Jump::parse(value).is_err(), "{value}");
    }
}

#[tokio::test]
async fn actual_ssh_config_preserves_nested_first_hop_and_overrides_later_hop_routes() {
    let fixture = Fixture::new();
    let config = fixture.0.join("config");
    std::fs::write(&config, "Host *\n  User fixture\n  IdentityFile none\n  IdentityAgent none\n  UserKnownHostsFile /dev/null\n  GlobalKnownHostsFile /dev/null\n  ControlMaster no\n  ControlPath none\nHost deep\n  HostName deep.example.invalid\n  Port 2200\nHost first\n  HostName first.example.invalid\n  ProxyJump deep\nHost last\n  HostName last.example.invalid\n  ProxyJump ignored\nHost ignored\n  ProxyCommand /bin/false\nHost single\n  HostName single.example.invalid\n  ProxyJump first\nHost multi\n  HostName multi.example.invalid\n  ProxyJump first,last\nHost excessive\n  ProxyJump deep,first,last,multi\nHost cycle\n  ProxyJump cycle\n").unwrap();
    for (name, expected) in [
        (
            "single",
            vec![
                "deep.example.invalid",
                "first.example.invalid",
                "single.example.invalid",
            ],
        ),
        (
            "multi",
            vec![
                "deep.example.invalid",
                "first.example.invalid",
                "last.example.invalid",
                "multi.example.invalid",
            ],
        ),
    ] {
        let mut resolver = Resolver {
            calls: 0,
            config: Some(config.clone()),
        };
        let mut output = Vec::new();
        resolver
            .walk(Jump::parse(name).unwrap(), None, &mut output)
            .await
            .unwrap();
        assert_eq!(
            output
                .iter()
                .map(|leg| leg.destination.hostname.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(output[0].destination.port, 2200);
        assert_eq!(resolver.calls, expected.len());
    }
    for name in ["excessive", "cycle", "ignored"] {
        let mut resolver = Resolver {
            calls: 0,
            config: Some(config.clone()),
        };
        assert!(resolver
            .walk(Jump::parse(name).unwrap(), None, &mut Vec::new())
            .await
            .is_err());
        assert!(resolver.calls <= 4);
    }
}

#[tokio::test]
async fn trusted_native_match_exec_stays_local_and_custom_proxy_is_not_executed() {
    let fixture = Fixture::new();
    let marker = fixture.0.join("match-ran");
    let proxy = fixture.0.join("proxy-ran");
    let config = fixture.0.join("config");
    std::fs::write(&config, format!("Match exec \"/usr/bin/touch {}\"\n  User fixture\nHost *\n  IdentityFile none\n  IdentityAgent none\n  ProxyCommand /usr/bin/touch {}\n", marker.display(), proxy.display())).unwrap();
    let mut resolver = Resolver {
        calls: 0,
        config: Some(config),
    };
    let mut output = Vec::new();
    assert!(resolver
        .walk(Jump::parse("synthetic").unwrap(), None, &mut output)
        .await
        .is_err());
    assert!(
        marker.exists(),
        "-G preserves trusted native Match exec semantics"
    );
    assert!(
        !proxy.exists(),
        "-G never executes a ProxyCommand before refusal"
    );
    assert!(output.is_empty());
}

fn key(seed: u8) -> ssh_key::PrivateKey {
    ssh_key::PrivateKey::new(
        KeypairData::Ed25519(Ed25519Keypair::from_seed(&[seed; 32])),
        "synthetic",
    )
    .unwrap()
}
fn public(seed: u8) -> Vec<u8> {
    key(seed).public_key().to_bytes().unwrap()
}
fn string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}
fn bind(leg: u8, id: &str, sequence: u64, session: &[u8], host: u8) -> SshRouteRequest {
    let mut bytes = vec![27];
    let signature = Vec::try_from(key(host).try_sign(session).unwrap()).unwrap();
    for value in [
        b"session-bind@openssh.com".as_slice(),
        &public(host),
        session,
        &signature,
    ] {
        string(&mut bytes, value);
    }
    bytes.push(0);
    SshRouteRequest::SessionBind {
        leg,
        connection_id: id.into(),
        request_id: sequence,
        packet: STANDARD.encode(bytes),
    }
}
#[derive(Clone)]
struct Agent(
    Arc<AtomicUsize>,
    Option<(Arc<tokio::sync::Semaphore>, Arc<tokio::sync::Semaphore>)>,
);
impl super::super::LocalAgent for Agent {
    type Connection = Self;
    async fn connect(&self) -> std::result::Result<Self, Failure> {
        Ok(self.clone())
    }
}
impl super::super::AgentConnection for Agent {
    async fn exchange(&mut self, packet: &[u8]) -> std::result::Result<Vec<u8>, Failure> {
        self.0.fetch_add(1, Ordering::SeqCst);
        if packet[0] == 27 {
            return Ok(vec![6]);
        }
        if let Some((entered, release)) = &self.1 {
            entered.add_permits(1);
            release.acquire().await.unwrap().forget();
        }
        let mut reader = super::super::Reader::new(packet)?;
        reader.byte_is(13)?;
        let user = reader.string()?;
        let seed = if user == public(2) { 2 } else { 4 };
        assert_eq!(user, public(seed));
        let data = reader.string()?;
        let signature = Vec::try_from(key(seed).try_sign(data).unwrap()).unwrap();
        let mut response = vec![14];
        string(&mut response, &signature);
        Ok(response)
    }
}
fn verifier(calls: Arc<AtomicUsize>) -> RouteVerifier<Agent> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut selections = Vec::new();
    for (name, host, user, port) in [
        ("jump.example.invalid", 1, "visitor", 2222),
        ("target.example.invalid", 3, "person", 22),
    ] {
        selections.push(chimaera_link::SshAuthGrantRequest {
            version: 1,
            keeper_boot: "boot".into(),
            destination: SshAuthDestination {
                hostname: name.into(),
                user: user.into(),
                port,
            },
            host_keys: vec![chimaera_link::SshAuthHostKey {
                key: STANDARD.encode(public(host)),
                is_ca: false,
            }],
            user_keys: vec![STANDARD.encode(public(host + 1))],
        });
    }
    let request = SshRouteGrantRequest {
        version: 1,
        keeper_boot: "boot".into(),
        destination: selections[1].destination.clone(),
        route: SshRoute {
            version: 1,
            jumps: vec![selections[0].destination.clone()],
        },
        legs: selections
            .iter()
            .map(|selected| SshRouteAuthLeg {
                policy: None,
                destination: selected.destination.clone(),
                mode: SshRouteMode::Key,
                host_keys: selected.host_keys.clone(),
                user_keys: selected.user_keys.clone(),
            })
            .collect(),
    };
    let legs = selections
        .iter()
        .map(|selected| {
            GrantVerifier::new(selected, deadline, Agent(calls.clone(), None))
                .ok()
                .unwrap()
        })
        .collect();
    RouteVerifier::new(request, legs, deadline).ok().unwrap()
}

#[tokio::test]
async fn cross_leg_trust_reuse_and_global_connection_budget_refuse_before_agent_effects() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut checked = verifier(calls.clone());
    assert!(checked
        .handle(bind(1, "wrong-leg", 1, b"s1", 1))
        .await
        .is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        checked
            .handle(bind(0, "jump", 2, b"s2", 1))
            .await
            .ok()
            .unwrap(),
        Some(SshRouteReply::Bound { leg: 0, .. })
    ));
    assert!(
        checked
            .handle(bind(1, "target", 3, b"s2", 3))
            .await
            .is_err(),
        "global session tombstone"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    checked
        .handle(SshRouteRequest::ConnectionClosed {
            leg: 0,
            connection_id: "jump".into(),
        })
        .await
        .ok()
        .unwrap();
    assert!(
        checked.handle(bind(1, "jump", 4, b"s4", 3)).await.is_err(),
        "global closed-connection tombstone"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let mut checked = verifier(calls.clone());
    for n in 1..=32u64 {
        let leg = (n % 2) as u8;
        checked
            .handle(bind(
                leg,
                &format!("c{n}"),
                n,
                &n.to_be_bytes(),
                if leg == 0 { 1 } else { 3 },
            ))
            .await
            .ok()
            .unwrap();
    }
    let before = calls.load(Ordering::SeqCst);
    assert!(checked
        .handle(bind(0, "overflow", 33, b"overflow", 1))
        .await
        .is_err());
    assert_eq!(calls.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn route_control_owner_loss_and_wrong_leg_close_the_whole_authentication_channel() {
    use futures_util::SinkExt;
    use tokio_tungstenite::{
        tungstenite::{protocol::Role, Message},
        WebSocketStream,
    };
    for cancelled in [false, true] {
        let (a, b) = tokio::io::duplex(256 * 1024);
        let (native, mut keeper) = tokio::join!(
            WebSocketStream::from_raw_socket(
                a,
                Role::Client,
                Some(chimaera_link::websocket_config(true))
            ),
            WebSocketStream::from_raw_socket(
                b,
                Role::Server,
                Some(chimaera_link::websocket_config(true))
            )
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let (owner, rx) = tokio::sync::watch::channel(false);
        let checked = verifier(calls.clone());
        let grant = grant(&checked.request);
        let task = tokio::spawn(super::super::control::run_route(checked, grant, native, rx));
        if cancelled {
            owner.send_replace(true);
        } else {
            keeper
                .send(Message::Text(
                    serde_json::to_string(&bind(1, "wrong", 1, b"s", 1))
                        .unwrap()
                        .into(),
                ))
                .await
                .unwrap();
        }
        let result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert!(
            result
                == Err(if cancelled {
                    Failure::Revoked
                } else {
                    Failure::InvalidBinding
                })
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

fn sign(leg: u8, id: &str, sequence: u64, user: &str, session: &[u8], host: u8) -> SshRouteRequest {
    let public = public(host + 1);
    let mut data = Vec::new();
    string(&mut data, session);
    data.push(50);
    for value in [
        user.as_bytes(),
        b"ssh-connection",
        b"publickey-hostbound-v00@openssh.com",
    ] {
        string(&mut data, value);
    }
    data.push(1);
    string(&mut data, b"ssh-ed25519");
    string(&mut data, &public);
    string(&mut data, &self::public(host));
    let mut packet = vec![13];
    string(&mut packet, &public);
    string(&mut packet, &data);
    packet.extend_from_slice(&0u32.to_be_bytes());
    SshRouteRequest::Sign {
        leg,
        connection_id: id.into(),
        request_id: sequence,
        packet: STANDARD.encode(packet),
    }
}

#[tokio::test]
async fn signing_keeps_the_original_user_key_host_session_and_global_request_order() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut checked = verifier(calls.clone());
    checked
        .handle(bind(0, "jump", 1, b"jump-session", 1))
        .await
        .ok()
        .unwrap();
    checked
        .handle(bind(1, "target", 2, b"target-session", 3))
        .await
        .ok()
        .unwrap();
    assert!(matches!(
        checked
            .handle(sign(0, "jump", 3, "person", b"jump-session", 1))
            .await
            .ok()
            .unwrap(),
        Some(SshRouteReply::Failure {
            leg: 0,
            error: Failure::InvalidRequest,
            ..
        })
    ));
    assert!(checked
        .handle(sign(1, "jump", 4, "visitor", b"jump-session", 1))
        .await
        .is_err());
    assert!(checked
        .handle(sign(0, "jump", 3, "visitor", b"jump-session", 1))
        .await
        .is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    // Failed authentication retires that connection. A separate fresh grant
    // demonstrates successful verification for both original native policies.
    let mut checked = verifier(calls.clone());
    checked
        .handle(bind(0, "jump", 1, b"jump-session", 1))
        .await
        .ok()
        .unwrap();
    checked
        .handle(bind(1, "target", 2, b"target-session", 3))
        .await
        .ok()
        .unwrap();
    assert!(matches!(
        checked
            .handle(sign(0, "jump", 5, "visitor", b"jump-session", 1))
            .await
            .ok()
            .unwrap(),
        Some(SshRouteReply::Signature { leg: 0, .. })
    ));
    assert!(matches!(
        checked
            .handle(sign(1, "target", 6, "person", b"target-session", 3))
            .await
            .ok()
            .unwrap(),
        Some(SshRouteReply::Signature { leg: 1, .. })
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 6);
    checked.deadline = Instant::now();
    assert!(matches!(
        checked
            .handle(sign(0, "jump", 7, "visitor", b"jump-session", 1))
            .await,
        Err(Failure::Expired)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 6);
}

#[tokio::test]
async fn each_leg_selects_its_own_native_agent_and_public_host_trust() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let fixture = Fixture::new();
    let mut effective = Vec::new();
    let mut agents = Vec::new();
    for (n, hostname, user, port) in [
        (1u8, "jump.example.invalid", "visitor", 2222),
        (3, "target.example.invalid", "person", 22),
    ] {
        let known = fixture.0.join(format!("known{n}"));
        let hostkey = key(n).public_key().to_openssh().unwrap();
        let hostlookup = if port == 22 {
            hostname.to_string()
        } else {
            format!("[{hostname}]:{port}")
        };
        std::fs::write(&known, format!("{hostlookup} {hostkey}\n")).unwrap();
        let socket = fixture.0.join(format!("agent{n}"));
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        agents.push(tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            assert_eq!(socket.read_u32().await.unwrap(), 1);
            assert_eq!(socket.read_u8().await.unwrap(), 11);
            let public = public(n + 1);
            let mut reply = vec![12];
            reply.extend_from_slice(&1u32.to_be_bytes());
            string(&mut reply, &public);
            string(&mut reply, b"synthetic");
            socket.write_u32(reply.len() as u32).await.unwrap();
            socket.write_all(&reply).await.unwrap();
            // Keep the synthetic responder alive for the post-reply kernel peer check.
            let mut extra = [0; 1];
            let eof =
                tokio::time::timeout(std::time::Duration::from_secs(3), socket.read(&mut extra))
                    .await
                    .expect("synthetic agent client did not close")
                    .expect("synthetic agent EOF read failed");
            assert_eq!(eof, 0, "unexpected extra synthetic agent request");
        }));
        let text = format!("hostname {hostname}\nuser {user}\nport {port}\npubkeyauthentication true\nidentitiesonly no\nhostkeyalgorithms ssh-ed25519\npubkeyacceptedalgorithms ssh-ed25519\ncasignaturealgorithms ssh-ed25519\nkexalgorithms curve25519-sha256\nciphers chacha20-poly1305@openssh.com\nmacs hmac-sha2-256-etm@openssh.com\nidentityagent {}\nuserknownhostsfile {}\nglobalknownhostsfile none\nproxyjump {}\n", socket.display(), known.display(), if n == 3 {"jump"} else {"none"});
        effective.push(Effective {
            text,
            destination: SshAuthDestination {
                hostname: hostname.into(),
                user: user.into(),
                port,
            },
        });
    }
    let selection = select(effective, &fixture.0, None, "boot".into()).await;
    for agent in agents {
        agent.await.unwrap();
    }
    let selection = selection.unwrap();
    assert_eq!(selection.request.legs.len(), 2);
    for (leg, seed) in selection.request.legs.iter().zip([1, 3]) {
        assert_eq!(leg.host_keys[0].key, STANDARD.encode(public(seed)));
        assert_eq!(leg.user_keys, vec![STANDARD.encode(public(seed + 1))]);
        assert!(leg.mode == SshRouteMode::Key);
    }
    assert_eq!(selection.request.route.jumps[0].port, 2222);
    assert_eq!(selection.request.destination.user, "person");
    // Reuse the selected native algorithm policies when constructing each verifier.
    assert!(selection
        .verifier(Instant::now() + Duration::from_secs(30))
        .is_ok());
}

fn grant(request: &SshRouteGrantRequest) -> chimaera_link::SshRouteGrant {
    chimaera_link::SshRouteGrant {
        policies: request.legs.iter().map(|leg| leg.policy.clone()).collect(),
        version: 1,
        grant_id: "synthetic".into(),
        expires_in: 180,
        destination: request.destination.clone(),
        route: request.route.clone(),
        modes: request.legs.iter().map(|leg| leg.mode).collect(),
    }
}

#[tokio::test]
async fn malformed_route_frames_cancel_an_earlier_touch_before_it_can_release_a_signature() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{
        tungstenite::{protocol::Role, Message},
        WebSocketStream,
    };
    for invalid in 0..4 {
        let (a, b) = tokio::io::duplex(256 * 1024);
        let (native, mut keeper) = tokio::join!(
            WebSocketStream::from_raw_socket(
                a,
                Role::Client,
                Some(chimaera_link::websocket_config(true))
            ),
            WebSocketStream::from_raw_socket(
                b,
                Role::Server,
                Some(chimaera_link::websocket_config(true))
            ),
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(tokio::sync::Semaphore::new(0));
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let mut checked = verifier(calls.clone());
        for leg in checked.legs.iter_mut().flatten() {
            leg.agent.1 = Some((entered.clone(), release.clone()));
        }
        let grant = grant(&checked.request);
        let (_owner, rx) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(super::super::control::run_route(checked, grant, native, rx));
        keeper
            .send(Message::Text(
                serde_json::to_string(&bind(0, "jump", 1, b"session", 1))
                    .unwrap()
                    .into(),
            ))
            .await
            .unwrap();
        let bound = tokio::time::timeout(Duration::from_secs(1), keeper.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(bound,Message::Text(ref text) if text.contains("bound")));
        keeper
            .send(Message::Text(
                serde_json::to_string(&sign(0, "jump", 2, "visitor", b"session", 1))
                    .unwrap()
                    .into(),
            ))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), entered.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        let mut invalid_frame = serde_json::to_value(bind(1, "target", 3, b"other", 3)).unwrap();
        match invalid {
            0 => invalid_frame["leg"] = serde_json::json!(2),
            1 => invalid_frame["packet"] = serde_json::json!("not-base64!"),
            2 => invalid_frame["connection_id"] = serde_json::json!("invalid/id"),
            _ => {
                invalid_frame["packet"] =
                    serde_json::json!(
                        STANDARD.encode(vec![0; chimaera_link::SSH_AUTH_PACKET_MAX + 1])
                    )
            }
        }
        keeper
            .send(Message::Text(
                serde_json::to_string(&invalid_frame).unwrap().into(),
            ))
            .await
            .unwrap();
        let result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert!(result == Err(Failure::InvalidRequest));
        // Failure is observed while touch is still withheld. Releasing it later
        // cannot resume the canceled local-agent future or write a signature.
        release.add_permits(1);
        let next = tokio::time::timeout(Duration::from_secs(1), keeper.next())
            .await
            .unwrap();
        assert!(!matches!(next, Some(Ok(Message::Text(_)))));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}

#[tokio::test]
async fn interactive_selection_requires_native_public_trust_and_never_downgrades_a_selected_agent()
{
    let fixture = Fixture::new();
    let known = fixture.0.join("known");
    std::fs::write(
        &known,
        format!(
            "target.example.invalid {}\n",
            key(3).public_key().to_openssh().unwrap()
        ),
    )
    .unwrap();
    let text = |agent: &str, trust: &Path| {
        format!("hostname target.example.invalid\nuser person\nport 22\npubkeyauthentication true\nidentitiesonly no\nhostkeyalgorithms ssh-ed25519\npubkeyacceptedalgorithms ssh-ed25519\ncasignaturealgorithms ssh-ed25519\nkexalgorithms curve25519-sha256\nciphers chacha20-poly1305@openssh.com\nmacs hmac-sha2-256-etm@openssh.com\nidentityagent {agent}\nuserknownhostsfile {}\nglobalknownhostsfile none\nproxyjump none\n", trust.display())
    };
    let effective = |text| {
        vec![Effective {
            text,
            destination: SshAuthDestination {
                hostname: "target.example.invalid".into(),
                user: "person".into(),
                port: 22,
            },
        }]
    };
    let selected = select(
        effective(text("none", &known)),
        &fixture.0,
        None,
        "boot".into(),
    )
    .await
    .unwrap();
    assert!(selected.request.legs[0].mode == SshRouteMode::Interactive);
    assert!(selected.request.legs[0].user_keys.is_empty());
    assert!(selected.legs[0].is_none());
    assert_eq!(
        selected.request.legs[0].host_keys[0].key,
        STANDARD.encode(public(3))
    );
    let mut checked = selected
        .verifier(Instant::now() + Duration::from_secs(1))
        .ok()
        .unwrap();
    assert!(
        checked
            .handle(bind(0, "interactive", 1, b"s", 3))
            .await
            .is_err(),
        "interactive policy cannot open a signing connection"
    );
    let missing = fixture.0.join("missing-agent");
    assert!(matches!(
        select(
            effective(text(missing.to_str().unwrap(), &known)),
            &fixture.0,
            None,
            "boot".into()
        )
        .await,
        Err(SelectionFailure::AgentUnavailable)
    ));
    for policy in [
        "pubkeyauthentication no",
        "pubkeyauthentication false",
        "pubkeyauthentication true\npreferredauthentications keyboard-interactive,password",
    ] {
        let password_only =
            text(missing.to_str().unwrap(), &known).replace("pubkeyauthentication true", policy);
        let selected = select(effective(password_only), &fixture.0, None, "boot".into())
            .await
            .unwrap();
        assert!(
            selected.request.legs[0].mode == SshRouteMode::Interactive,
            "no agent enumeration is allowed for {policy}"
        );
    }
    use chimaera_link::SshRouteMethod::{KeyboardInteractive, Password};
    for (policy, expected) in [
        ("passwordauthentication no", vec![KeyboardInteractive]),
        ("kbdinteractiveauthentication no", vec![Password]),
        ("preferredauthentications password", vec![Password]),
        (
            "preferredauthentications password,keyboard-interactive",
            vec![Password, KeyboardInteractive],
        ),
    ] {
        let restricted = format!("{}{}\n", text("none", &known), policy);
        let selected = select(effective(restricted), &fixture.0, None, "boot".into())
            .await
            .unwrap();
        assert!(
            selected.request.legs[0].policy.as_ref().unwrap().methods == expected,
            "disabled methods and exact preference order survive: {policy}"
        );
    }
    for policy in [
        "passwordauthentication unsupported",
        "preferredauthentications keyboard-interactive,publickey,password",
        "gssapiauthentication yes",
    ] {
        let restricted = format!("{}{}\n", text(missing.to_str().unwrap(), &known), policy);
        assert!(
            matches!(
                select(effective(restricted), &fixture.0, None, "boot".into()).await,
                Err(SelectionFailure::UnsupportedConfiguration)
            ),
            "unsupported policy cannot widen: {policy}"
        );
    }
    let no_interaction = format!(
        "{}passwordauthentication no\nkbdinteractiveauthentication no\n",
        text("none", &known)
    );
    assert!(matches!(
        select(effective(no_interaction), &fixture.0, None, "boot".into()).await,
        Err(SelectionFailure::UnsupportedConfiguration)
    ));
    let no_trust = fixture.0.join("missing-trust");
    assert!(matches!(
        select(
            effective(text("none", &no_trust)),
            &fixture.0,
            None,
            "boot".into()
        )
        .await,
        Err(SelectionFailure::HostTrustRequired)
    ));
}

#[tokio::test]
async fn only_a_verified_signature_enables_mfa_for_its_original_leg() {
    let registry = super::super::lifecycle::Registry::default();
    let attempt = registry.admit(0).ok().unwrap();
    let mut checked = verifier(Arc::new(AtomicUsize::new(0)));
    let receipt = grant(&checked.request);
    let owner = attempt
        .bind_route("host", &receipt, &checked.request, checked.deadline)
        .ok()
        .unwrap();
    let auth = |leg: usize| chimaera_link::SshRoutePromptAuth {
        grant_id: receipt.grant_id.clone(),
        keeper_boot: checked.request.keeper_boot.clone(),
        leg: leg as u8,
        mode: checked.request.legs[leg].mode,
        destination: checked.request.legs[leg].destination.clone(),
    };
    let first = auth(0);
    let last = auth(1);
    checked.attach_prompts(owner.proof());
    checked
        .handle(bind(0, "jump", 1, b"jump-session", 1))
        .await
        .ok()
        .unwrap();
    assert!(
        registry.route_prompt(0, "host", &first).is_none(),
        "binding alone is not a signature receipt"
    );
    assert!(matches!(
        checked
            .handle(sign(0, "jump", 2, "visitor", b"jump-session", 1))
            .await
            .ok()
            .unwrap(),
        Some(SshRouteReply::Signature { leg: 0, .. })
    ));
    let guard = registry.route_prompt(0, "host", &first).unwrap();
    assert!(guard.active());
    assert!(
        registry.route_prompt(0, "host", &last).is_none(),
        "one leg cannot approve another leg's MFA"
    );
    drop(checked);
    assert!(
        !guard.active(),
        "control verifier loss revokes an already displayed prompt"
    );
}
