//! Synthetic Mac loader fixture: unchanged production modules, no Tauri prompt claim.
#[cfg(target_os = "macos")]
#[path = "../ssh_agent_fixture_support/keeper.rs"]
mod keeper;
#[cfg(target_os = "macos")]
#[path = "../ssh_agent_fixture_support/route.rs"]
mod route;
#[cfg(target_os = "macos")]
#[path = "../ssh_agent_fixture_support/source.rs"]
mod source;
#[cfg(target_os = "macos")]
#[allow(dead_code, unused_imports)]
#[path = "../ssh_agent.rs"]
mod ssh_agent;
#[cfg(target_os = "macos")]
use std::{
    io::{Read, Write},
    path::PathBuf,
    sync::Arc,
};
#[cfg(target_os = "macos")]
fn askpass() -> i32 {
    use std::os::unix::net::UnixStream;
    let Some(path) = std::env::var_os("CHIMAERA_ASKPASS_SOCK") else {
        return 2;
    };
    let Ok(alias) = std::env::var(chimaera_remote::ASKPASS_ALIAS_ENV) else {
        return 2;
    };
    if alias.len() > 128 || alias.contains('\n') {
        return 2;
    }
    let Ok(mut socket) = UnixStream::connect(path) else {
        return 2;
    };
    if socket
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .is_err()
        || socket
            .set_write_timeout(Some(std::time::Duration::from_secs(5)))
            .is_err()
    {
        return 2;
    }
    let challenge = std::env::args().nth(2).unwrap_or_default();
    if challenge.len() > 8192 {
        return 2;
    }
    if socket
        .write_all(format!("chimaera-askpass-scope-v1\n{alias}\n{challenge}").as_bytes())
        .is_err()
    {
        return 2;
    }
    let _ = socket.shutdown(std::net::Shutdown::Write);
    let mut response = zeroize::Zeroizing::new(Vec::with_capacity(16385));
    if socket.take(16385).read_to_end(&mut response).is_err() || response.len() > 16384 {
        return 2;
    }
    if std::io::stdout().write_all(&response).is_err() {
        return 2;
    }
    0
}
#[cfg(target_os = "macos")]
async fn fixture(path: PathBuf, action: String) -> Result<(), ()> {
    use base64::Engine;
    use ssh_agent::{
        key_agent::{Admission, Agent, SelectedFile, Session},
        lifecycle::Registry,
        trust::Owner,
    };
    let registry = Registry::default();
    let attempt = registry.admit(0).map_err(|_| ())?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let (cancel_ready, mut cancel_seen) = tokio::sync::watch::channel(false);
    let prompt_action = action.clone();
    let owner = Owner {
        alias: "synthetic-local-loader".into(),
        guard: attempt.native_prompt(deadline),
        account: Arc::default(),
        current: Arc::new(|| true),
        prompt: Arc::new(move |prompt, guard| {
            let action = prompt_action.clone();
            let cancel_ready = cancel_ready.clone();
            Box::pin(async move {
                assert!(prompt.host_key.is_none());
                assert!(guard.active());
                println!("PROMPT");
                std::io::stdout().flush().map_err(|_| ()).ok()?;
                // The runner captures exact helper/group ownership before it
                // permits any answer; no timing guess or private stdin data.
                use tokio::io::AsyncReadExt;
                let mut receipt = [0; 9];
                if tokio::io::stdin().read_exact(&mut receipt).await.is_err()
                    || &receipt != b"CONTINUE\n"
                    || !guard.active()
                {
                    return None;
                }
                if action == "cancel" {
                    // Cancel only after the runner positively captured every
                    // in-flight helper identity, rather than racing a timer.
                    let _ = cancel_ready.send(true);
                }
                match action.as_str() {
                    "decline" => None,
                    "wrong" => Some("synthetic-wrong-passphrase".into()),
                    "pause" | "deadline" | "cancel" => std::future::pending().await,
                    _ => Some("fixture-only-passphrase".into()),
                }
            })
        }),
    };
    let admission = Admission::acquire().map_err(|_| ())?;
    let file = SelectedFile::capture(path).map_err(|_| ())?.ok_or(())?;
    let session = Session::spawn(&owner, deadline, &admission)
        .await
        .map_err(|_| ())?;
    let public = if action == "cancel" {
        let loading = session.load(&file, &owner, deadline);
        tokio::pin!(loading);
        tokio::select! {
            result = &mut loading => result.map_err(|_| ())?,
            changed = cancel_seen.changed() => {
                changed.map_err(|_| ())?;
                if !*cancel_seen.borrow() {
                    return Err(());
                }
                // Dropping the real production load future retires its
                // observer while the original five-second owner stays valid.
                return Err(());
            }
        }
    } else {
        session
            .load(&file, &owner, deadline)
            .await
            .map_err(|_| ())?
    };
    // Two exact constrained adds of an identical public key are both proven,
    // even though the private agent's identity count does not increase.
    let duplicate = session
        .load(&file, &owner, deadline)
        .await
        .map_err(|_| ())?;
    if duplicate != public {
        return Err(());
    }
    let _backend = Agent::selected(
        None,
        &[],
        Some(session),
        &[(public.clone(), file)],
        owner,
        deadline,
    )
    .map_err(|_| ())?;
    println!(
        "PUBLIC {}",
        base64::engine::general_purpose::STANDARD.encode(public)
    );
    std::io::stdout().flush().map_err(|_| ())?;
    Ok(())
}
#[cfg(target_os = "macos")]
fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    match args.get(1).map(String::as_str) {
        Some("--native-key-agent") => std::process::exit(ssh_agent::key_agent::run_helper()),
        Some("--askpass") => std::process::exit(askpass()),
        Some("--key-load-fixture") => {
            if args.len() != 4
                || !matches!(
                    args[3].as_str(),
                    "accept" | "wrong" | "decline" | "cancel" | "deadline" | "pause"
                )
            {
                std::process::exit(2)
            }
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            let result = runtime.block_on(fixture(PathBuf::from(&args[2]), args[3].clone()));
            runtime.block_on(async {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            });
            if result.is_err() {
                println!("REFUSED");
                std::process::exit(2)
            }
        }
        Some("--source-packets") => {
            if args.len() != 3 || source::prepare(std::path::Path::new(&args[2])).is_err() {
                std::process::exit(2)
            }
        }
        Some("--source-fixture") => {
            if args.len() != 5 || !matches!(args[4].as_str(), "accept" | "refuse") {
                std::process::exit(2)
            }
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            let result = runtime.block_on(source::run(
                PathBuf::from(&args[2]),
                args[3].clone(),
                args[4].clone(),
            ));
            runtime.block_on(async {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            });
            if result.is_err() {
                println!("SOURCE_FAILED");
                std::process::exit(2)
            }
        }
        Some("--keeper-route-fixture") => {
            if args.len() != 5
                || !matches!(
                    args[4].as_str(),
                    "accept"
                        | "refuse"
                        | "cancel"
                        | "deadline"
                        | "password-accept"
                        | "password-wrong"
                        | "password-decline"
                        | "password-cancel"
                        | "password-deadline"
                )
            {
                std::process::exit(2)
            }
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            let result = runtime.block_on(keeper::run(
                PathBuf::from(&args[2]),
                args[3].clone(),
                args[4].clone(),
            ));
            runtime.block_on(async {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            });
            if result.is_err() {
                println!("KEEPER_FAILED");
                std::process::exit(2);
            }
        }
        Some("--route-fixture") => {
            if args.len() != 5
                || !matches!(
                    args[4].as_str(),
                    "accept" | "grant-expiry" | "grant-cancel" | "ready-expiry" | "config-change"
                )
            {
                std::process::exit(2)
            }
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            let result = runtime.block_on(route::run(
                PathBuf::from(&args[2]),
                args[3].clone(),
                args[4].clone(),
            ));
            runtime.block_on(async {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            });
            if result.is_err() {
                println!("ROUTE_FAILED");
                std::process::exit(2)
            }
        }
        _ => std::process::exit(2),
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {
    // The target remains compilable in all-target CI; runtime proof is macOS-only.
    std::process::exit(2);
}
