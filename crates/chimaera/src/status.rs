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
        Some(m) if !m.written_here() => report_elsewhere(&m, "this node"),
        Some(m) if m.is_alive() => report_running(&m),
        Some(m) => report_stale(&m),
    }
    Ok(())
}

async fn remote(host: &str, home: RemoteHome) -> anyhow::Result<()> {
    // The same one-exec probe `connect` uses: manifest + liveness in a
    // single ssh round trip, sent through `sh -c` so a tcsh/fish login
    // shell on the host reads it. A manifest another login node wrote is
    // reported as such, not judged from this one (connect routes there).
    match chimaera_remote::remote_probe(host, home).await? {
        None => println!("not running"),
        Some(p) if !p.here() => report_elsewhere(&p.manifest, &p.node),
        Some(p) if p.alive => report_running(&p.manifest),
        Some(p) => report_stale(&p.manifest),
    }
    Ok(())
}

/// A manifest another node wrote: its pid names a process there, so from
/// `node` it is neither running nor stale — only registered.
fn report_elsewhere(m: &Manifest, node: &str) {
    println!(
        "registered on {} (pid {}, 127.0.0.1:{} there, build {}) — {node} can't check it; \
         `chimaera connect` routes to that node",
        m.hostname,
        m.pid,
        m.port,
        m.build.as_deref().unwrap_or("pre-build-id")
    );
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
