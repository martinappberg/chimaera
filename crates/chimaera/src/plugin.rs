//! `chimaera plugin list|add|update|remove|trust|untrust|caps|activity` —
//! the daemon's plugin routes from the command line, against the daemon
//! running on this node (its manifest names the port and the token). Thin
//! by design: the daemon downloads (or copies a local build), verifies the
//! checksums, decides whether it may run and swaps `current`; this prints
//! what it did in one line, as the card would say it (no hashes: only a
//! failed check is news, and the daemon refuses it).
//!
//! A plugin the maintainers haven't verified (or an update that asks for
//! more) comes back as a 409 carrying what it can do: this prints that list,
//! as the card's trust prompt shows it, asks, and sends the answer back as
//! the capability digest the user saw (`--trust` answers yes for scripts).

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
/// A refusal is an error with the daemon's words.
async fn call(method: &str, path: &str, body: Option<&Value>) -> anyhow::Result<Value> {
    let (code, value) = request(method, path, body).await?;
    if !(200..300).contains(&code) {
        return Err(refused(code, &value));
    }
    Ok(value)
}

/// A refusal in the daemon's own words (its `{error}`), whatever the status:
/// they say what to do instead (a plugin now built in says where it went).
fn refused(code: u16, value: &Value) -> anyhow::Error {
    match value.get("error").and_then(Value::as_str) {
        Some(error) => anyhow::anyhow!("{error}"),
        None => anyhow::anyhow!("the daemon answered {code}"),
    }
}

/// `call`, answering the daemon's status and body whatever they are.
async fn request(method: &str, path: &str, body: Option<&Value>) -> anyhow::Result<(u16, Value)> {
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
    Ok((code, value))
}

/// `text` safe to print on a terminal: a plugin's words (its name, its
/// Can lines) never carry an escape that redraws the prompt around them.
fn plain(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// What a trust prompt shows, in lines: who asks, from where, and what it
/// can do (or, for an update, what it would do beyond the running version).
fn trust_text(error: &str, t: &Value) -> String {
    let mut out = vec![error.to_string()];
    out.push(format!(
        "{} {} from {}{}",
        s(t, "name"),
        s(t, "version"),
        s(t, "source"),
        if s(t, "tier") == "privileged" {
            " — it runs programs on this host"
        } else {
            ""
        }
    ));
    let lines = |key: &str| -> Vec<String> {
        t.get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|l| l.get("text").and_then(Value::as_str))
                    .map(|l| format!("  - {l}"))
                    .collect()
            })
            .unwrap_or_default()
    };
    let grown = lines("grown");
    if !grown.is_empty() {
        out.push(format!(
            "It would also (beyond {}):",
            t.get("from_version")
                .and_then(Value::as_str)
                .unwrap_or("the running version")
        ));
        out.extend(grown);
    }
    out.push("It can:".into());
    let can = lines("can");
    if can.is_empty() {
        out.push("  - nothing beyond running in its sandbox".into());
    }
    out.extend(can);
    out.iter().map(|l| plain(l)).collect::<Vec<_>>().join("\n")
}

/// Ask on the terminal: yes, or (for a plugin that runs programs) its
/// name typed out. Without a terminal to ask on, the answer is no.
fn ask(t: &Value) -> anyhow::Result<bool> {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        bail!("not trusted: no terminal to ask on — rerun with --trust to trust what it can do");
    }
    let confirm = t.get("confirm").and_then(Value::as_str);
    match confirm {
        Some(name) => print!("Type {} to trust it: ", plain(name)),
        None => print!("Trust it? [y/N] "),
    }
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let line = line.trim();
    Ok(match confirm {
        Some(name) => line == name,
        None => matches!(line, "y" | "Y" | "yes" | "Yes"),
    })
}

/// A change that may come back asking for trust: shown, asked (or
/// `trust` answers yes), and sent again with the digest the user saw.
async fn with_trust(
    method: &str,
    path: &str,
    mut body: Value,
    trust: bool,
) -> anyhow::Result<Value> {
    let (code, value) = request(method, path, Some(&body)).await?;
    if code == 409 && value.get("trust").is_some_and(Value::is_object) {
        let t = &value["trust"];
        println!("{}", trust_text(s(&value, "error"), t));
        if !trust && !ask(t)? {
            bail!("not trusted: nothing was installed or changed");
        }
        body["trust"] = t["caps"].clone();
        return call(method, path, Some(&body)).await;
    }
    if !(200..300).contains(&code) {
        return Err(refused(code, &value));
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

/// The list's leading mark: `✓` for a Chimaera plugin (the curated lock
/// names it), as the card's badge; blank otherwise.
fn mark(p: &Value) -> &'static str {
    if p["first_party"] == true {
        "✓"
    } else {
        " "
    }
}

/// A catalog entry after its version, as the card says it: where it stands.
fn list_line(p: &Value) -> String {
    let mut parts = Vec::new();
    let id = s(p, "id");
    let pinned = p.get("pinned_version").and_then(Value::as_str);
    match s(p, "source") {
        "available" => {
            parts.push("available".to_string());
            parts.push(format!("install with: chimaera plugin add {id}"));
        }
        "installed" => {
            parts.push("installed".to_string());
            if p.get("local_path").is_some_and(Value::is_string) {
                parts.push("local build".to_string());
            }
            if s(p, "tier") == "privileged" {
                parts.push("runs programs".to_string());
            }
            match p["hold"].get("kind").and_then(Value::as_str) {
                Some("untrusted") => parts.push(format!(
                    "waiting for your trust: chimaera plugin trust {id}"
                )),
                Some("blocked") => {
                    parts.push(format!("blocked by chimaera: {}", s(&p["hold"], "reason")))
                }
                Some("policy") => parts.push(format!("off: {}", s(&p["hold"], "reason"))),
                _ => {}
            }
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
        println!(
            "{} {:<18} {:<9} {}",
            mark(p),
            s(p, "id"),
            s(p, "version"),
            list_line(p)
        );
        if let Some(update) = p.get("update").and_then(|u| u.get("version")) {
            println!(
                "{:<30} update available: {} (`chimaera plugin update {}`)",
                "",
                update.as_str().unwrap_or(""),
                s(p, "id")
            );
        }
        if let Some(fault) = p.get("fault").and_then(Value::as_str) {
            println!("{:<30} off: {fault}", "");
        }
    }
    Ok(())
}

/// What an install or update printed: one line, "installed mycelium
/// 0.2.1" (a local build says so; an update names the version it replaced,
/// which the daemon keeps for Use previous).
fn installed_line(verb: &str, body: &Value) -> String {
    let local = if body["plugin"]
        .get("local_path")
        .is_some_and(Value::is_string)
    {
        " (local build)"
    } else {
        ""
    };
    let was = body
        .get("previous")
        .and_then(Value::as_str)
        .filter(|_| verb == "updated")
        .map(|p| format!(" (was {p})"))
        .unwrap_or_default();
    format!(
        "{verb} {} {}{local}{was}",
        s(body, "id"),
        s(body, "version")
    )
}

/// `chimaera plugin add <id | owner/repo> [--version x]` or `--path <dir>`.
pub async fn add(
    plugin: Option<&str>,
    version: Option<&str>,
    path: Option<&Path>,
    trust: bool,
) -> anyhow::Result<()> {
    let body = match (plugin, path) {
        (_, Some(dir)) => {
            // The daemon runs on this node: the same filesystem, an
            // absolute path.
            let dir = std::path::absolute(dir).with_context(|| format!("{}", dir.display()))?;
            with_trust("POST", "/plugins/install", json!({"path": dir}), trust).await?
        }
        (Some(repo), None) if repo.contains('/') => {
            with_trust(
                "POST",
                "/plugins/install",
                json!({"github": repo, "version": version}),
                trust,
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
    println!("{}", installed_line("installed", &body));
    Ok(())
}

pub async fn update(id: &str, trust: bool) -> anyhow::Result<()> {
    let body = with_trust(
        "POST",
        &format!("/plugins/{}/update", id_segment(id)?),
        json!({}),
        trust,
    )
    .await?;
    println!("{}", installed_line("updated", &body));
    Ok(())
}

/// `chimaera plugin trust <id>`: an installed plugin waiting for trust.
pub async fn trust(id: &str, yes: bool) -> anyhow::Result<()> {
    let path = format!("/plugins/{}/trust", id_segment(id)?);
    // The daemon answers an empty ask with what there is to trust.
    with_trust("POST", &path, json!({}), yes).await?;
    println!("trusted {id}");
    Ok(())
}

/// `chimaera plugin untrust <id>`.
pub async fn untrust(id: &str) -> anyhow::Result<()> {
    call(
        "DELETE",
        &format!("/plugins/{}/trust", id_segment(id)?),
        None,
    )
    .await?;
    println!("withdrew your trust in {id}: it is off everywhere until you trust it again");
    Ok(())
}

/// `chimaera plugin caps <plugin.toml>`: no daemon, the host's own reading.
pub fn caps(manifest: &Path, as_json: bool) -> anyhow::Result<()> {
    let text =
        std::fs::read_to_string(manifest).with_context(|| format!("{}", manifest.display()))?;
    let v = chimaera_server::plugin_capabilities(&text).map_err(anyhow::Error::msg)?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    println!("{} {} · {}", s(&v, "id"), s(&v, "version"), s(&v, "tier"));
    println!("caps {}", s(&v, "caps"));
    for line in v["can"].as_array().into_iter().flatten() {
        println!("  - {}", s(line, "text"));
    }
    Ok(())
}

/// "just now" · "5 min ago" · "3 h ago" · "2 d ago".
fn ago(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", s / 60),
        3600..86400 => format!("{} h ago", s / 3600),
        _ => format!("{} d ago", s / 86400),
    }
}

/// `chimaera plugin activity <id>`: newest first, one line each.
pub async fn activity(id: &str) -> anyhow::Result<()> {
    let body = call(
        "GET",
        &format!("/plugins/{}/activity", id_segment(id)?),
        None,
    )
    .await?;
    let entries = body["entries"].as_array().cloned().unwrap_or_default();
    if entries.is_empty() {
        println!("nothing recorded for {id}");
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    for e in &entries {
        let rest: Vec<String> = e
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(k, v)| *k != "kind" && *k != "ts" && !v.is_null())
            .map(|(k, v)| {
                format!(
                    "{k}={}",
                    v.as_str().map(str::to_string).unwrap_or(v.to_string())
                )
            })
            .collect();
        let ago = ago(now.saturating_sub(e["ts"].as_u64().unwrap_or(now)));
        println!("{ago:>9}  {:<12} {}", s(e, "kind"), rest.join(" "));
    }
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
    fn a_trust_prompt_prints_no_terminal_escapes() {
        let ask = json!({"name": "Evil\u{1b}[2K\u{1b}[1AGood", "version": "1.0.0",
            "source": "github.com/acme/evil", "tier": "privileged",
            "can": [{"text": "Runs sh\u{1b}[8m: hidden"}]});
        let text = trust_text("needs trust", &ask);
        assert!(!text.contains('\u{1b}'), "{text:?}");
        assert!(text.contains("Evil?[2K?[1AGood"));
    }

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
        assert_eq!(id_segment("mycelium").unwrap(), "mycelium");
        for bad in ["", "Agent", "a/b", "x?y", "a b", "../x"] {
            assert!(id_segment(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_list_line_says_what_the_card_says() {
        let first_party = json!({"id": "latex", "version": "0.1.2", "source": "installed",
            "first_party": true, "verified": true, "pinned_version": "0.1.2"});
        assert_eq!(mark(&first_party), "✓");
        assert_eq!(list_line(&first_party), "installed");
        assert_eq!(
            list_line(
                &json!({"id": "latex", "version": "0.1.3", "source": "installed",
                "first_party": true, "verified": true, "pinned_version": "0.1.2",
                "previous": "0.1.2"})
            ),
            "installed · previous 0.1.2 · chimaera pins 0.1.2"
        );
        let available = json!({"id": "mycelium", "version": "0.1.1", "source": "available",
            "first_party": true, "verified": false, "pinned_version": "0.1.1"});
        assert_eq!(
            mark(&available),
            "✓",
            "the badge holds before the install too"
        );
        assert_eq!(
            list_line(&available),
            "available · install with: chimaera plugin add mycelium"
        );
        let local = json!({"id": "dev", "version": "0.2.0", "source": "installed",
            "first_party": false, "verified": false, "local_path": "/home/me/dev"});
        assert_eq!(mark(&local), " ");
        assert_eq!(list_line(&local), "installed · local build");
        assert_eq!(
            list_line(
                &json!({"id": "x", "version": "1.0.0", "source": "installed",
                "first_party": false, "verified": true})
            ),
            "installed",
            "a third-party copy: no badge, no tags"
        );
    }

    #[test]
    fn an_install_is_one_line_without_hashes() {
        let body = json!({"id": "mycelium", "version": "0.1.2", "previous": null,
            "sha256": {"plugin.wasm": "a".repeat(64), "plugin.toml": "b".repeat(64)},
            "plugin": {"id": "mycelium", "first_party": true}});
        assert_eq!(
            installed_line("installed", &body),
            "installed mycelium 0.1.2"
        );
        let body = json!({"id": "mycelium", "version": "0.1.3", "previous": "0.1.2",
            "plugin": {"id": "mycelium"}});
        assert_eq!(
            installed_line("updated", &body),
            "updated mycelium 0.1.3 (was 0.1.2)"
        );
        let body = json!({"id": "dev", "version": "0.2.0", "previous": "0.1.0",
            "plugin": {"local_path": "/home/me/dev"}});
        assert_eq!(
            installed_line("installed", &body),
            "installed dev 0.2.0 (local build)"
        );
    }

    /// `chimaera plugin add agent-notes` (built into Chimaera now) prints the
    /// daemon's refusal as it words it; a bare status only without one.
    #[test]
    fn a_refusal_is_the_daemons_own_words() {
        let retired = json!({"error": "Built into Chimaera now: Agent communication (Settings → Agents). Remove this copy."});
        assert_eq!(
            refused(409, &retired).to_string(),
            "Built into Chimaera now: Agent communication (Settings → Agents). Remove this copy."
        );
        assert_eq!(
            refused(502, &Value::Null).to_string(),
            "the daemon answered 502"
        );
    }

    #[test]
    fn remove_says_how_a_chimaera_plugin_comes_back() {
        let body = json!({"id": "mycelium", "removed": true,
            "plugin": {"source": "available"}});
        assert_eq!(
            removed_line("mycelium", &body),
            "removed mycelium — `chimaera plugin add mycelium` installs it again"
        );
        let body = json!({"id": "x", "removed": true, "plugin": null});
        assert_eq!(removed_line("x", &body), "removed x");
    }
}
