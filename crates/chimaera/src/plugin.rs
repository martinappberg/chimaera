//! `chimaera plugin list|add|update|remove` — the daemon's plugin routes
//! from the command line, against the daemon running on this node (its
//! manifest names the port and the token). Thin by design: the daemon
//! downloads (or copies a local build), verifies the checksums and swaps
//! `current`; this prints what it did, checksums included.

use std::path::Path;
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

/// A plugin id as a URL segment: what the daemon's ids are (lowercase
/// letters, digits, dashes), so nothing else reaches the URL.
fn id_segment(id: &str) -> anyhow::Result<&str> {
    let ok = !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !ok {
        bail!("{id:?} is not a plugin id (lowercase letters, digits and dashes)");
    }
    Ok(id)
}

/// What the card says about a copy: `chimaera` for a Chimaera plugin,
/// `verified` when its files match its release's checksums, `unverified ·
/// local` for a local build without them.
fn tags(p: &Value) -> Vec<String> {
    let mut tags = Vec::new();
    if p["first_party"] == true && p["source"] != "available" {
        tags.push("chimaera".to_string());
    }
    if p["verified"] == true {
        tags.push("verified".to_string());
    } else if p.get("local_path").is_some_and(Value::is_string) {
        tags.push("unverified · local".to_string());
    }
    tags
}

/// A catalog entry after its version, as the card says it: its tags, then
/// where it stands.
fn list_line(p: &Value) -> String {
    let mut parts = tags(p);
    let id = s(p, "id");
    let pinned = p.get("pinned_version").and_then(Value::as_str);
    match s(p, "source") {
        "available" => {
            parts.push("available".to_string());
            if let Some(pinned) = pinned {
                parts.push(format!("chimaera pins {pinned}"));
            }
            parts.push(format!("install with: chimaera plugin add {id}"));
        }
        "installed" => {
            parts.push("installed".to_string());
            if let Some(prev) = p.get("previous").and_then(Value::as_str) {
                parts.push(format!("previous {prev}"));
            }
            if let Some(pinned) = pinned.filter(|v| *v != s(p, "version")) {
                parts.push(format!("chimaera pins {pinned}"));
            }
        }
        // A daemon from before this CLI: say what it said.
        other => parts.push(other.to_string()),
    }
    parts.join(" · ")
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
        println!("{:<18} {:<9} {}", s(p, "id"), s(p, "version"), list_line(p));
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

/// What an install or update printed: the version, the checksums the daemon
/// verified the download against, and what the card now says.
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
    let tags = tags(&body["plugin"]);
    if !tags.is_empty() {
        println!("  {}", tags.join(" · "));
    }
}

/// `chimaera plugin add <id | owner/repo> [--version x]` or `--path <dir>`.
pub async fn add(
    plugin: Option<&str>,
    version: Option<&str>,
    path: Option<&Path>,
) -> anyhow::Result<()> {
    let body = match (plugin, path) {
        (_, Some(dir)) => {
            // The daemon runs on this node: the same filesystem, an
            // absolute path.
            let dir = std::path::absolute(dir).with_context(|| format!("{}", dir.display()))?;
            call("POST", "/plugins/install", Some(&json!({"path": dir}))).await?
        }
        (Some(repo), None) if repo.contains('/') => {
            call(
                "POST",
                "/plugins/install",
                Some(&json!({"github": repo, "version": version})),
            )
            .await?
        }
        (Some(id), None) => {
            if version.is_some() {
                bail!(
                    "--version needs a repository (owner/repo): `chimaera plugin add {id}` installs the release chimaera pins"
                );
            }
            call(
                "POST",
                &format!("/plugins/{}/install", id_segment(id)?),
                None,
            )
            .await?
        }
        (None, None) => bail!("name a plugin (an id, or owner/repo) or --path <dir>"),
    };
    print_installed("installed", &body);
    println!("  switch it on per workspace from the Extensions tab");
    Ok(())
}

pub async fn update(id: &str) -> anyhow::Result<()> {
    let body = call(
        "POST",
        &format!("/plugins/{}/update", id_segment(id)?),
        None,
    )
    .await?;
    print_installed("updated", &body);
    Ok(())
}

pub async fn remove(id: &str) -> anyhow::Result<()> {
    let body = call("DELETE", &format!("/plugins/{}", id_segment(id)?), None).await?;
    println!("{}", removed_line(id, &body));
    Ok(())
}

/// What Remove says: a Chimaera plugin can be installed again by its id.
fn removed_line(id: &str, body: &Value) -> String {
    if body["plugin"]["source"] == "available" {
        format!("removed {id} — `chimaera plugin add {id}` installs it again")
    } else {
        format!("removed {id}")
    }
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
    fn ids_are_the_only_url_segments() {
        assert_eq!(id_segment("agent-notes").unwrap(), "agent-notes");
        for bad in ["", "Agent", "a/b", "x?y", "a b", "../x"] {
            assert!(id_segment(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_list_line_says_what_the_card_says() {
        assert_eq!(
            list_line(
                &json!({"id": "agent-notes", "version": "0.1.0", "source": "installed",
                "first_party": true, "verified": true, "pinned_version": "0.1.0"})
            ),
            "chimaera · verified · installed"
        );
        assert_eq!(
            list_line(
                &json!({"id": "agent-notes", "version": "0.1.1", "source": "installed",
                "first_party": true, "verified": true, "pinned_version": "0.1.0",
                "previous": "0.1.0"})
            ),
            "chimaera · verified · installed · previous 0.1.0 · chimaera pins 0.1.0"
        );
        assert_eq!(
            list_line(
                &json!({"id": "mycelium", "version": "0.1.0", "source": "available",
                "first_party": true, "verified": false, "pinned_version": "0.1.0"})
            ),
            "available · chimaera pins 0.1.0 · install with: chimaera plugin add mycelium"
        );
        assert_eq!(
            list_line(
                &json!({"id": "dev", "version": "0.2.0", "source": "installed",
                "first_party": false, "verified": false, "local_path": "/home/me/dev"})
            ),
            "unverified · local · installed"
        );
        assert_eq!(
            list_line(
                &json!({"id": "x", "version": "1.0.0", "source": "installed",
                "first_party": false, "verified": false})
            ),
            "installed",
            "a copy installed before SHA256SUMS were kept"
        );
    }

    #[test]
    fn remove_says_how_a_chimaera_plugin_comes_back() {
        let body = json!({"id": "agent-notes", "removed": true,
            "plugin": {"source": "available"}});
        assert_eq!(
            removed_line("agent-notes", &body),
            "removed agent-notes — `chimaera plugin add agent-notes` installs it again"
        );
        let body = json!({"id": "x", "removed": true, "plugin": null});
        assert_eq!(removed_line("x", &body), "removed x");
    }
}
