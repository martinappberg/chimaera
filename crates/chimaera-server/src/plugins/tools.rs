//! The MCP tools workbench plugins contribute. Named in each manifest's
//! `provides.mcp_tools`; served by `mcp.rs` ONLY to sessions whose workspace
//! has the plugin active (tools/list AND the call gate), with the plugin's
//! instruction paragraph appended at initialize.
//!
//! Generic: every plugin answers through its component's exports
//! (`runtime`), the definitions checked against its manifest there.
//!
//! Pictures: a line of an answer that is exactly `![alt](output:<path>)`,
//! naming a PNG, JPEG, WebP or GIF in the answering plugin's own output
//! folder for the call's workspace, reaches the agent as an MCP `image`
//! block in that place (`answer_content`); the line stays as its caption,
//! with the file's absolute path, so an agent whose client drops images can
//! still open it. Read through the output folder's no-link reads, at most
//! `PICTURES_MAX` a call and `PICTURE_MAX` / `PICTURES_BYTES` in size; a
//! picture that can't be shown stays a line saying why.

use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};

use super::Manifest;
use crate::AppState;

/// The plugin that owns an MCP tool name, if any: an `active` one first
/// (two plugins may name the same tool; the one switched on here answers),
/// else any in the catalog (for the "isn't switched on" refusal).
pub(crate) fn owner(
    state: &AppState,
    active: &[Arc<Manifest>],
    tool: &str,
) -> Option<Arc<Manifest>> {
    let owns = |m: &&Arc<Manifest>| m.provides.mcp_tools.iter().any(|t| t == tool);
    if let Some(m) = active.iter().find(owns) {
        return Some(m.clone());
    }
    super::catalog(state).iter().find(owns).cloned()
}

/// What the active `plugins` add to what an agent in `ws` is handed: their
/// instruction paragraphs and their tool definitions, in catalog order. A
/// plugin that can't answer (refused, faulted, trapping) adds nothing.
pub(crate) async fn offered(
    state: &Arc<AppState>,
    plugins: &[Arc<Manifest>],
    ws: &str,
) -> (Vec<String>, Vec<Value>) {
    let mut paragraphs = Vec::new();
    let mut tools = Vec::new();
    for m in plugins {
        match state.plugin_runtime.offer(state, m, ws).await {
            Ok(offer) => {
                paragraphs.extend(offer.instructions);
                // The call gate selects the first active owner. Offer only
                // that owner's definition too, including when it is faulted.
                tools.extend(offer.tools.into_iter().filter(|tool| {
                    tool["name"].as_str().is_some_and(|name| {
                        owner(state, plugins, name).is_some_and(|owner| owner.id == m.id)
                    })
                }));
            }
            Err(err) => tracing::warn!(plugin = %m.id, %err, "plugin offers nothing"),
        }
    }
    (paragraphs, tools)
}

/// Dispatch a plugin tool (the caller already checked `m` is active in the
/// session's workspace).
pub(crate) async fn call(
    state: &Arc<AppState>,
    m: &Manifest,
    agent_id: &str,
    name: &str,
    args: &Value,
) -> Value {
    let Some(ws) = super::workspace_of_session(state, agent_id) else {
        return json!({
            "content": [{ "type": "text", "text": "this session has no workspace" }],
            "isError": true,
        });
    };
    state
        .plugin_runtime
        .call_tool(state, m, &ws, agent_id, name, args)
        .await
}

/// Pictures one answer may carry, each picture's size, and all together.
pub(crate) const PICTURES_MAX: usize = 8;
pub(crate) const PICTURE_MAX: usize = 2 << 20;
pub(crate) const PICTURES_BYTES: usize = 8 << 20;

/// `![alt](output:<path>)` alone on a line (surrounding blanks allowed).
fn picture_line(line: &str) -> Option<(&str, &str)> {
    let rest = line.trim().strip_prefix("![")?;
    let (alt, rest) = rest.split_once("](")?;
    let path = rest.strip_suffix(')')?;
    let path = path.strip_prefix("output:")?;
    (!alt.contains(']') && !path.is_empty() && !path.contains(['(', ')'])).then_some((alt, path))
}

/// The picture's media type, from its first bytes; the name's extension
/// must agree (a `.png` that is a JPEG is refused, not guessed).
fn media_type(name: &str, bytes: &[u8]) -> Option<&'static str> {
    let ext = Path::new(name).extension()?.to_str()?.to_ascii_lowercase();
    let sniffed = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "image/gif"
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else {
        return None;
    };
    let named = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => return None,
    };
    (named == sniffed).then_some(sniffed)
}

/// One named picture's media type and bytes, or why it can't be shown,
/// given how many (and how many bytes) the answer already carries.
fn picture(
    dir: &Path,
    path: &str,
    shown: usize,
    bytes_total: usize,
) -> Result<(&'static str, Vec<u8>), String> {
    let rel = super::output::relative(path)?;
    if shown >= PICTURES_MAX {
        return Err(format!("over {PICTURES_MAX} pictures in one answer"));
    }
    let data = super::output::read(dir, &rel, 0, PICTURE_MAX + 1)?;
    if data.len() > PICTURE_MAX {
        return Err(format!("over {} MiB", PICTURE_MAX >> 20));
    }
    if bytes_total + data.len() > PICTURES_BYTES {
        return Err(format!(
            "the answer's pictures pass {} MiB",
            PICTURES_BYTES >> 20
        ));
    }
    let mime = media_type(path, &data).ok_or_else(|| "not a PNG, JPEG, WebP or GIF".to_string())?;
    Ok((mime, data))
}

/// A plugin tool's answer as MCP content (blocking: reads the pictures it
/// names from `dir`, the plugin's output folder for this workspace). Text
/// without picture lines is one text block, exactly as before.
pub(crate) fn answer_content(dir: &Path, text: &str) -> Vec<Value> {
    use base64::Engine as _;
    let mut content = Vec::new();
    let mut pending = String::new();
    let (mut shown, mut bytes_total) = (0usize, 0usize);
    let flush = |pending: &mut String, content: &mut Vec<Value>| {
        if !pending.trim().is_empty() {
            content.push(json!({ "type": "text", "text": std::mem::take(pending) }));
        }
        pending.clear();
    };
    let mut fenced = false;
    for line in text.split_inclusive('\n') {
        // A picture line quoted inside a code block is an example, not a picture.
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        }
        let Some((alt, path)) = picture_line(line).filter(|_| !fenced) else {
            pending.push_str(line);
            continue;
        };
        let eol = if line.ends_with('\n') { "\n" } else { "" };
        let abs = match super::output::relative(path) {
            Ok(rel) => dir.join(rel),
            Err(_) => dir.join(path),
        };
        match picture(dir, path, shown, bytes_total) {
            Ok((mime, data)) => {
                shown += 1;
                bytes_total += data.len();
                pending.push_str(&format!("![{alt}]({}){eol}", abs.display()));
                flush(&mut pending, &mut content);
                content.push(json!({
                    "type": "image",
                    "data": base64::engine::general_purpose::STANDARD.encode(&data),
                    "mimeType": mime,
                }));
            }
            Err(why) => {
                pending.push_str(&format!("{alt}: {} (not shown: {why}){eol}", abs.display()));
            }
        }
    }
    flush(&mut pending, &mut content);
    if content.is_empty() {
        content.push(json!({ "type": "text", "text": text }));
    }
    content
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    fn temp(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "chimaera-tools-test-{}-{label}-{}",
            std::process::id(),
            &chimaera_core::generate_token()[..8]
        ));
        std::fs::create_dir_all(dir.join("shots")).unwrap();
        dir
    }

    #[test]
    fn an_answer_without_pictures_is_one_text_block_as_before() {
        let dir = temp("plain");
        let text = "built: 0 errors\n![not ours](https://example.com/x.png)\n\
                    ```md\n![example](output:shots/s1.png)\n```\n";
        std::fs::write(dir.join("shots/s1.png"), PNG).unwrap();
        assert_eq!(
            answer_content(&dir, text),
            vec![json!({"type": "text", "text": text})]
        );
    }

    #[test]
    fn a_picture_line_becomes_an_image_in_its_place_with_its_path_as_caption() {
        let dir = temp("shown");
        std::fs::write(dir.join("shots/s1.png"), PNG).unwrap();
        let content = answer_content(
            &dir,
            "Slide 1:\n![slide 1](output:shots/s1.png)\nlooks fine",
        );
        assert_eq!(content.len(), 3, "{content:?}");
        let caption = content[0]["text"].as_str().unwrap();
        assert!(caption.starts_with("Slide 1:\n![slide 1]("), "{caption}");
        assert!(caption.contains(&dir.join("shots/s1.png").display().to_string()));
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["mimeType"], "image/png");
        use base64::Engine as _;
        let data = base64::engine::general_purpose::STANDARD
            .decode(content[1]["data"].as_str().unwrap())
            .unwrap();
        assert_eq!(data, PNG);
        assert_eq!(content[2], json!({"type": "text", "text": "looks fine"}));
    }

    // Symlinks need privileges on Windows.
    #[cfg(unix)]
    #[test]
    fn a_picture_that_cannot_be_shown_stays_a_line_saying_why() {
        let dir = temp("refused");
        let outside = temp("refused-outside");
        std::fs::write(outside.join("secret.png"), PNG).unwrap();
        std::os::unix::fs::symlink(outside.join("secret.png"), dir.join("shots/link.png")).unwrap();
        std::fs::write(dir.join("shots/fake.png"), b"GIF89a....").unwrap();
        std::fs::write(dir.join("shots/big.png"), vec![0u8; PICTURE_MAX + 1]).unwrap();
        let text = [
            "![missing](output:shots/none.png)",
            "![escape](output:../x.png)",
            "![link](output:shots/link.png)",
            "![mislabelled](output:shots/fake.png)",
            "![big](output:shots/big.png)",
        ]
        .join("\n");
        let content = answer_content(&dir, &text);
        assert_eq!(content.len(), 1, "{content:?}");
        let said = content[0]["text"].as_str().unwrap();
        assert_eq!(said.matches("(not shown: ").count(), 5, "{said}");
        assert!(said.contains("not a PNG, JPEG, WebP or GIF"), "{said}");
        assert!(said.contains("over 2 MiB"), "{said}");
    }

    #[test]
    fn an_answer_carries_at_most_eight_pictures() {
        let dir = temp("many");
        let mut text = String::new();
        for i in 0..10 {
            std::fs::write(dir.join(format!("shots/{i}.png")), PNG).unwrap();
            text.push_str(&format!("![{i}](output:shots/{i}.png)\n"));
        }
        let content = answer_content(&dir, &text);
        let images = content.iter().filter(|c| c["type"] == "image").count();
        assert_eq!(images, PICTURES_MAX);
        let last = content.last().unwrap()["text"].as_str().unwrap();
        assert_eq!(last.matches("over 8 pictures").count(), 2, "{last}");
    }
}
