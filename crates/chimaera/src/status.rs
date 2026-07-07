use chimaera_core::Manifest;

pub async fn run(host: Option<&str>) -> anyhow::Result<()> {
    match host {
        None => local(),
        Some(host) => remote(host).await,
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

async fn remote(host: &str) -> anyhow::Result<()> {
    let Some(manifest) = chimaera_remote::remote_manifest(host).await? else {
        println!("not running");
        return Ok(());
    };
    if chimaera_remote::remote_alive(host, manifest.pid).await? {
        report_running(&manifest);
    } else {
        report_stale(&manifest);
    }
    Ok(())
}

fn report_running(m: &Manifest) {
    println!(
        "running: 127.0.0.1:{} (pid {}, v{})",
        m.port, m.pid, m.version
    );
}

fn report_stale(m: &Manifest) {
    println!("stale manifest (pid {} dead)", m.pid);
}
