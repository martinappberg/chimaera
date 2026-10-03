//! Actual selected Session and external agent; only the keeper protocol is simulated.
use crate::ssh_agent::{
    connect,
    lifecycle::Registry,
    route::ConfigContext,
    trust::{self, Owner},
    Failure,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use chimaera_link::SshRouteRequest;
use signature::Signer;
use ssh_key::private::{Ed25519Keypair, KeypairData};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{io::AsyncReadExt, time::Instant};

fn string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}
fn host(seed: u8) -> Result<ssh_key::PrivateKey, ()> {
    ssh_key::PrivateKey::new(
        KeypairData::Ed25519(Ed25519Keypair::from_seed(&[seed; 32])),
        "synthetic-source-fixture",
    )
    .map_err(|_| ())
}
fn packets(leg: u8, public: &[u8]) -> Result<(String, [SshRouteRequest; 2]), ()> {
    let host = host(41 + leg)?;
    let host_public = host.public_key().to_bytes().map_err(|_| ())?;
    let session = [71 + leg; 32];
    let connection = format!("fixture-source-{leg}");
    let signature = Vec::try_from(host.try_sign(&session).map_err(|_| ())?).map_err(|_| ())?;
    let mut bind = vec![27];
    for value in [
        b"session-bind@openssh.com".as_slice(),
        &host_public,
        &session,
        &signature,
    ] {
        string(&mut bind, value);
    }
    bind.push(0);
    let mut data = Vec::new();
    string(&mut data, &session);
    data.push(50);
    for value in [
        b"fixture".as_slice(),
        b"ssh-connection",
        b"publickey-hostbound-v00@openssh.com",
    ] {
        string(&mut data, value);
    }
    data.push(1);
    string(&mut data, b"ssh-ed25519");
    string(&mut data, public);
    string(&mut data, &host_public);
    let mut sign = vec![13];
    string(&mut sign, public);
    string(&mut sign, &data);
    sign.extend_from_slice(&0u32.to_be_bytes());
    Ok((
        STANDARD.encode(host_public),
        [
            SshRouteRequest::SessionBind {
                leg,
                connection_id: connection.clone(),
                request_id: u64::from(leg) * 2 + 1,
                packet: STANDARD.encode(bind),
            },
            SshRouteRequest::Sign {
                leg,
                connection_id: connection,
                request_id: u64::from(leg) * 2 + 2,
                packet: STANDARD.encode(sign),
            },
        ],
    ))
}

/// Only synthetic host private keys are constructed here. The user input is a
/// bounded PUBLIC identity; actual user private loading remains production code.
pub(super) fn prepare(root: &Path) -> Result<(), ()> {
    let directory = std::fs::symlink_metadata(root).map_err(|_| ())?;
    let uid = unsafe { nix::libc::geteuid() };
    if !root.is_absolute()
        || !directory.is_dir()
        || directory.uid() != uid
        || directory.mode() & 0o7777 != 0o700
    {
        return Err(());
    }
    let path = root.join("source-user-public");
    let input = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(&path)
        .map_err(|_| ())?;
    let info = input.metadata().map_err(|_| ())?;
    if !info.is_file() || info.uid() != uid || info.mode() & 0o7777 != 0o600 || info.len() > 8192 {
        return Err(());
    }
    let mut encoded = Vec::new();
    input.take(8193).read_to_end(&mut encoded).map_err(|_| ())?;
    if encoded.is_empty() || encoded.len() > 8192 {
        return Err(());
    }
    let public = STANDARD.decode(encoded).map_err(|_| ())?;
    let key = ssh_key::PublicKey::from_bytes(&public).map_err(|_| ())?;
    if key.algorithm() != ssh_key::Algorithm::Ed25519 {
        return Err(());
    }
    let (hop, first) = packets(0, &public)?;
    let (target, second) = packets(1, &public)?;
    let bytes = serde_json::to_vec(&serde_json::json!({
        "version": 1, "public": STANDARD.encode(public), "hosts": [hop, target],
        "requests": [first[0], first[1], second[0], second[1]],
    }))
    .map_err(|_| ())?;
    if bytes.len() > 16384 {
        return Err(());
    }
    // Exclusive fixture output only; never overwrite a user's path.
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(root.join("source-packets.json"))
        .map_err(|_| ())?;
    output.write_all(&bytes).map_err(|_| ())?;
    let after = std::fs::symlink_metadata(root).map_err(|_| ())?;
    if (after.dev(), after.ino()) != (directory.dev(), directory.ino()) {
        return Err(());
    }
    Ok(())
}

pub(super) async fn run(config: PathBuf, endpoint: String, action: String) -> Result<(), ()> {
    let url = url::Url::parse(&endpoint).map_err(|_| ())?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(());
    }
    let home = config.parent().ok_or(())?.to_path_buf();
    let context = ConfigContext::fixture(config, home).map_err(|_| ())?;
    let registry = Registry::default();
    let attempt = registry.admit(0).map_err(|_| ())?;
    let deadline = Instant::now() + Duration::from_secs(15);
    let owner = Owner {
        alias: "source-fixture".into(),
        guard: attempt.native_prompt(deadline),
        account: Arc::default(),
        current: Arc::new(|| true),
        prompt: Arc::new(|prompt, guard| {
            Box::pin(async move {
                if prompt.host_key.is_some() || !guard.active() {
                    return None;
                }
                println!("SOURCE_PROMPT");
                std::io::stdout().flush().ok()?;
                let mut receipt = [0; 9];
                if tokio::io::stdin().read_exact(&mut receipt).await.ok()? != 9
                    || &receipt != b"CONTINUE\n"
                    || !guard.active()
                {
                    return None;
                }
                Some("fixture-only-passphrase".into())
            })
        }),
    };
    let client = chimaera_link::Client::new(
        &endpoint,
        Some(chimaera_link::Tokens {
            access_token: "synthetic-route-device-token".into(),
            refresh_token: "synthetic-route-refresh-token".into(),
            token_type: "Bearer".into(),
            expires_in: 3600,
        }),
    )
    .map_err(|_| ())?;
    let caps = tokio::time::timeout_at(deadline, async {
        client.me().await?;
        client.ssh_auth_capabilities().await
    })
    .await
    .map_err(|_| ())?
    .map_err(|_| ())?;
    if !caps.route_policy_supported() {
        return Err(());
    }
    let selection = trust::resolve_fixture("source-fixture", caps.keeper_boot, owner, context)
        .await
        .map_err(|_| ())?;
    if selection.fixture_deadline() != Some(deadline)
        || selection.request.legs.len() != 2
        || selection
            .request
            .legs
            .iter()
            .any(|leg| leg.mode != chimaera_link::SshRouteMode::Key || leg.user_keys.len() != 1)
        || selection.request.legs[0].user_keys != selection.request.legs[1].user_keys
    {
        return Err(());
    }
    println!("SOURCE_SELECTED");
    std::io::stdout().flush().map_err(|_| ())?;
    let result =
        connect::authenticate_route(&client, "fixture-host", selection, attempt, || async {
            Ok(())
        })
        .await;
    match (action.as_str(), result) {
        ("accept", Ok(())) => println!("SOURCE_ACCEPTED"),
        ("refuse", Err(Failure::AgentRefused)) => println!("SOURCE_REFUSED external"),
        _ => return Err(()),
    }
    let held = (0..3)
        .map(|_| registry.admit(0).map_err(|_| ()))
        .collect::<Result<Vec<_>, _>>()?;
    let restored = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(attempt) = registry.admit(0) {
                return attempt;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| ())?;
    drop((held, restored));
    Ok(())
}
