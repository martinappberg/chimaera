//! `chimaera plugin list|add|update|remove` — the daemon's installed-plugin
//! routes from the command line, against the daemon running on this node
//! (its manifest names the port and the token). Thin by design: the daemon
//! downloads, verifies the checksums and swaps `current`; this prints what
//! it did, checksums included.

use std::process::Stdio;

use anyhow::{bail, Context};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

/// The local daemon's manifest, when a daemon runs on this node.
fn daemon() -> anyhow::Result<chimaera_core::Manifest> {
    match chimaera_core::Manifest::load()? {
        Some(m) if !m.written_here() => bail!(
            "the chimaera daemon is registered on {} — run this there",
            m.hostname
        ),
        Some(m) if m.is_alive() => Ok(m),
        _ => bail!("no chimaera daemon is running here — start one with `chimaera serve`"),
    }
}

/// A string inside a curl config file's double quotes.
fn curl_quoted(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// One request to the local daemon through the system `curl` (the daemon's
/// own transport). The bearer token and the body ride curl's config on
/// stdin (`--config -`), never argv: a login node shows every user's argv.
async fn call(method: &str, path: &str, body: Option<&Value>) -> anyhow::Result<Value> {
    let m = daemon()?;
    let url = format!("http://127.0.0.1:{}/api/v1{path}", m.port);
    let mut config = format!(
        "header = \"Authorization: Bearer {}\"\n",
        curl_quoted(&m.token)
    );
    if let Some(body) = body {
        config.push_str("header = \"Content-Type: application/json\"\n");
        config.push_str(&format!("data = \"{}\"\n", curl_quoted(&body.to_string())));
    }
    // 5 minutes: an install fetches the release, its checksums and a
    // component of up to 16 MiB (the daemon bounds each step itself).
    let mut child = tokio::process::Command::new("curl")
        .args(["-sS", "-m", "300", "--config", "-", "-X", method])
        .args(["-w", "\n%{http_code}", &url])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("failed to run curl")?;
    let mut stdin = child.stdin.take().context("curl has no stdin")?;
    stdin.write_all(config.as_bytes()).await?;
    drop(stdin);
    let out = child.wait_with_output().await.context("waiting for curl")?;
    if !out.status.success() {
        bail!(
            "could not reach the daemon: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, code) = text.rsplit_once('\n').context("no status from curl")?;
    let code: u16 = code.trim().parse().context("no status from curl")?;
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    if !(200..300).contains(&code) {
        match value.get("error").and_then(Value::as_str) {
            Some(error) => bail!("{error}"),
            None => bail!("the daemon answered {code}"),
        }
    }
    Ok(value)
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Where a catalog entry comes from, as the card says it.
fn source_line(p: &Value) -> String {
    let mut line = match s(p, "source") {
        "installed" => match p.get("embedded_version").and_then(Value::as_str) {
            Some(e) => format!("installed · {e} ships with chimaera"),
            None => "installed".to_string(),
        },
        _ => match p.get("installed_version").and_then(Value::as_str) {
            Some(i) if p["stale"] == true => {
                format!("ships with chimaera · installed {i} is older (stale)")
            }
            Some(i) => format!("ships with chimaera · installed {i} too"),
            None => "ships with chimaera".to_string(),
        },
    };
    if let Some(prev) = p.get("previous").and_then(Value::as_str) {
        line.push_str(&format!(" · previous {prev}"));
    }
    line
}

pub async fn list() -> anyhow::Result<()> {
    let body = call("GET", "/plugins", None).await?;
    let plugins = body
        .get("plugins")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if plugins.is_empty() {
        println!("no plugins on this daemon");
        return Ok(());
    }
    for p in &plugins {
        println!(
            "{:<18} {:<9} {}",
            s(p, "id"),
            s(p, "version"),
            source_line(p)
        );
        if let Some(update) = p.get("update").and_then(|u| u.get("version")) {
            println!(
                "{:<28} update available: {} (`chimaera plugin update {}`)",
                "",
                update.as_str().unwrap_or(""),
                s(p, "id")
            );
        }
        if let Some(fault) = p.get("fault").and_then(Value::as_str) {
            println!("{:<28} off: {fault}", "");
        }
    }
    Ok(())
}

/// What an install or update printed: the version and the checksums the
/// daemon verified the download against.
fn print_installed(verb: &str, body: &Value) {
    let previous = body
        .get("previous")
        .and_then(Value::as_str)
        .map(|p| format!(" (was {p}; the daemon keeps it for Use previous)"))
        .unwrap_or_default();
    println!("{verb} {} {}{previous}", s(body, "id"), s(body, "version"));
    for file in ["plugin.wasm", "plugin.toml"] {
        println!(
            "  {file:<12} sha256 {}",
            body["sha256"][file].as_str().unwrap_or("?")
        );
    }
}

pub async fn add(github: &str, version: Option<&str>) -> anyhow::Result<()> {
    let body = call(
        "POST",
        "/plugins/install",
        Some(&json!({"github": github, "version": version})),
    )
    .await?;
    print_installed("installed", &body);
    println!("  switch it on per workspace from the Plugins tab");
    Ok(())
}

pub async fn update(id: &str) -> anyhow::Result<()> {
    let body = call("POST", &format!("/plugins/{id}/update"), None).await?;
    print_installed("updated", &body);
    Ok(())
}

pub async fn remove(id: &str) -> anyhow::Result<()> {
    let body = call("DELETE", &format!("/plugins/{id}"), None).await?;
    match body.get("plugin").filter(|p| !p.is_null()) {
        Some(p) => println!(
            "removed the installed {id} — the {} that ships with chimaera runs now",
            s(p, "version")
        ),
        None => println!("removed {id}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_quoting_keeps_json_intact() {
        let body = json!({"github": "a/b", "note": "say \"hi\" \\ bye"}).to_string();
        let quoted = curl_quoted(&body);
        assert!(!quoted.contains('\n'));
        // curl's config unquoting reverses exactly these two escapes.
        let back = quoted.replace("\\\"", "\"").replace("\\\\", "\\");
        assert_eq!(back, body);
    }

    #[test]
    fn the_source_line_says_what_the_card_says() {
        assert_eq!(
            source_line(&json!({"source": "embedded"})),
            "ships with chimaera"
        );
        assert_eq!(
            source_line(
                &json!({"source": "installed", "embedded_version": "0.3.1", "previous": "0.3.0"})
            ),
            "installed · 0.3.1 ships with chimaera · previous 0.3.0"
        );
        assert_eq!(
            source_line(
                &json!({"source": "embedded", "installed_version": "0.2.0", "stale": true})
            ),
            "ships with chimaera · installed 0.2.0 is older (stale)"
        );
    }
}
