use anyhow::Context;
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
    let output = tokio::process::Command::new("ssh")
        .arg(host)
        .arg("cat ~/.chimaera/manifest.json")
        .output()
        .await
        .context("failed to run ssh")?;
    if !output.status.success() {
        println!("not running");
        return Ok(());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let manifest: Manifest = match serde_json::from_str(text.trim()) {
        Ok(m) => m,
        Err(e) => {
            tracing::debug!("unparseable remote manifest: {e}");
            println!("not running");
            return Ok(());
        }
    };
    let alive = tokio::process::Command::new("ssh")
        .arg(host)
        .arg(format!("kill -0 {}", manifest.pid))
        .output()
        .await
        .context("failed to run ssh")?
        .status
        .success();
    if alive {
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
