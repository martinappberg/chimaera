use chimaera_core::Manifest;
use chimaera_remote::RemoteHome;

pub async fn run(host: Option<&str>) -> anyhow::Result<()> {
    match host {
        None => local(),
        // The build picks the home (dev → ~/.chimaera-dev), same as connect:
        // a dev status reports the daemon a dev connect would talk to.
        Some(host) => remote(host, RemoteHome::current()).await,
    }
}

fn local() -> anyhow::Result<()> {
    match Manifest::load()? {
        None => println!("not running"),
        Some(m) if m.is_alive() => report_running(&m),
        Some(m) => report_stale(&m),
    }
    Ok(())
}

async fn remote(host: &str, home: RemoteHome) -> anyhow::Result<()> {
    // The same one-exec probe `connect` uses: manifest + liveness in a
    // single ssh round trip, sent through `sh -c` so a tcsh/fish login
    // shell on the host reads it.
    match chimaera_remote::remote_probe(host, home).await? {
        None => println!("not running"),
        Some((manifest, true)) => report_running(&manifest),
        Some((manifest, false)) => report_stale(&manifest),
    }
    Ok(())
}

fn report_running(m: &Manifest) {
    println!(
        "running: 127.0.0.1:{} (pid {}, v{}, build {})",
        m.port,
        m.pid,
        m.version,
        m.build.as_deref().unwrap_or("pre-build-id")
    );
}

fn report_stale(m: &Manifest) {
    println!("stale manifest (pid {} dead)", m.pid);
}
