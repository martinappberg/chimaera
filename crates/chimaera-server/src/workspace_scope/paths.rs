//! Path aliases are presentation only; callers validate the mapped resource.
use anyhow::{ensure, Result};
use base64::Engine;
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

pub const HEADER: &str = "x-chimaera-viewer-root";
#[derive(Clone, Debug)]
pub struct Alias {
    pub root: PathBuf,
    pub viewer: PathBuf,
}
impl Alias {
    pub fn decode(encoded: &str, root: PathBuf) -> Result<Self> {
        ensure!(encoded.len() <= 5462, "viewer root exceeds limit");
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(encoded)?;
        ensure!(
            bytes.len() <= 4096
                && base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes) == encoded,
            "invalid viewer root encoding"
        );
        let raw = String::from_utf8(bytes)?;
        let viewer = PathBuf::from(&raw);
        ensure!(
            viewer.is_absolute()
                && !raw.chars().any(char::is_control)
                && !viewer.components().any(|c| c == Component::ParentDir),
            "invalid viewer root"
        );
        Ok(Self { root, viewer })
    }
    pub fn input(&self, raw: &str) -> String {
        replace(raw, &self.viewer, &self.root)
    }
    pub fn output(&self, raw: &str) -> String {
        replace(raw, &self.root, &self.viewer)
    }
    pub fn request_body(&self, body: &mut Value) -> std::collections::HashMap<String, String> {
        let mut keys = std::collections::HashMap::new();
        for key in ["path", "from", "to", "base", "dir"] {
            if let Some(value) = body.get_mut(key) {
                map_string(value, |p| self.input(p));
            }
        }
        for key in ["bases", "candidates"] {
            if let Some(values) = body[key].as_array_mut() {
                for value in values {
                    map_string(value, |p| self.input(p));
                }
            }
        }
        if let Some(targets) = body.get_mut("targets").and_then(Value::as_array_mut) {
            for value in targets {
                if let Some(original) = value.as_str() {
                    if let Some(decoded) = crate::embed::target_path(original) {
                        let mapped = self.input(&decoded);
                        if mapped != decoded {
                            let encoded = encode_target(&mapped);
                            keys.insert(encoded.clone(), original.to_owned());
                            *value = Value::String(encoded);
                        }
                    }
                }
            }
        }
        keys
    }
    /// Restore the caller's exact resolver keys, including original escapes.
    pub fn restore_keys(body: &mut Value, keys: &std::collections::HashMap<String, String>) {
        if let Some(results) = body.get_mut("results").and_then(Value::as_object_mut) {
            let old = std::mem::take(results);
            for (key, value) in old {
                results.insert(keys.get(&key).cloned().unwrap_or(key), value);
            }
        }
    }
    /// Never traverse arbitrary content: notebooks, document text, table rows,
    /// prompts and journals can themselves contain fields named `path`.
    pub fn response(&self, path: &str, body: &mut Value) {
        match path {
            "/git/status" | "/git/diff" | "/git/worktrees" | "/git/repos" | "/git/branches"
            | "/git/log" | "/git/show" | "/git/compare" => {
                self.field(body, "toplevel");
                // A log's `path` is repository-relative; output leaves it alone.
                self.field(body, "path");
                for key in ["entries", "files", "worktrees", "repos"] {
                    if let Some(rows) = body.get_mut(key).and_then(Value::as_array_mut) {
                        for row in rows {
                            for field in ["path", "orig", "parent"] {
                                self.field(row, field);
                            }
                        }
                    }
                }
            }
            "/workspaces" => {
                if let Some(rows) = body.as_array_mut() {
                    for row in rows {
                        self.field(row, "root");
                    }
                }
            }
            "/sessions" => {
                if let Some(rows) = body.as_array_mut() {
                    for row in rows {
                        self.session(row);
                    }
                }
            }
            "/fs/dirs" | "/fs/list" => {
                self.field(body, "path");
                self.field(body, "parent");
                for key in ["entries", "dirs"] {
                    if let Some(rows) = body[key].as_array_mut() {
                        for row in rows {
                            self.field(row, "path");
                            self.field(row, "target");
                        }
                    }
                }
            }
            "/fs/home" | "/fs/mkdir" | "/fs/create" | "/fs/rename" | "/fs/copy" | "/fs/move"
            | "/fs/draft" | "/fs/drafts" => {
                self.field(body, "path");
                if let Some(rows) = body.get_mut("drafts").and_then(Value::as_array_mut) {
                    for row in rows {
                        self.field(row, "path");
                    }
                }
                if let Some(rows) = body.as_array_mut() {
                    for row in rows {
                        self.field(row, "path");
                    }
                }
            }
            "/fs/validate" | "/fs/resolve_targets" => {
                for key in ["valid", "ambiguous", "results"] {
                    if let Some(map) = body[key].as_object_mut() {
                        let old = std::mem::take(map);
                        for (key, mut value) in old {
                            self.field(&mut value, "path");
                            if let Some(values) = value.as_array_mut() {
                                for value in values {
                                    self.field(value, "path");
                                }
                            }
                            map.insert(self.output(&key), value);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    pub fn session(&self, row: &mut Value) {
        for key in ["cwd", "cwd_current", "workspace_root"] {
            self.field(row, key);
        }
    }
    fn field(&self, row: &mut Value, key: &str) {
        if let Some(value) = row.get_mut(key) {
            map_string(value, |p| self.output(p));
        }
    }
}
fn encode_target(path: &str) -> String {
    let mut result = String::new();
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            result.push(b as char);
        } else {
            result.push_str(&format!("%{b:02X}"));
        }
    }
    result
}
fn map_string(value: &mut Value, map: impl FnOnce(&str) -> String) {
    if let Some(raw) = value.as_str() {
        *value = Value::String(map(raw));
    }
}
fn replace(raw: &str, from: &Path, to: &Path) -> String {
    Path::new(raw)
        .strip_prefix(from)
        .map(|relative| {
            if relative.as_os_str().is_empty() {
                to.to_string_lossy().into_owned()
            } else {
                to.join(relative).to_string_lossy().into_owned()
            }
        })
        .unwrap_or_else(|_| raw.to_owned())
}
pub fn encode_query(values: &[(String, String)]) -> String {
    fn encode(value: &str) -> String {
        let mut result = String::new();
        for b in value.bytes() {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                result.push(b as char);
            } else {
                result.push_str(&format!("%{b:02X}"));
            }
        }
        result
    }
    values
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}
#[cfg(test)]
mod tests {
    use super::*;
    fn alias() -> Alias {
        Alias::decode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("/Users/å/project"),
            PathBuf::from("/cloud/project"),
        )
        .unwrap()
    }
    #[test]
    fn alias_is_component_bound_and_does_not_modify_contents() {
        let alias = alias();
        assert_eq!(alias.input("/Users/å/project/file"), "/cloud/project/file");
        assert_eq!(
            alias.input("/Users/å/project-other/file"),
            "/Users/å/project-other/file"
        );
        assert_eq!(
            alias.output("/cloud/project-other/file"),
            "/cloud/project-other/file"
        );
        let mut value = serde_json::json!({"path":"/cloud/project/file","text":"/cloud/project/secret","data":{"path":"/cloud/project/content"}});
        alias.response("/fs/draft", &mut value);
        assert_eq!(value["path"], "/Users/å/project/file");
        assert_eq!(value["text"], "/cloud/project/secret");
        assert_eq!(value["data"]["path"], "/cloud/project/content");
        let original = value.clone();
        alias.response("/fs/notebook", &mut value);
        assert_eq!(value, original);
    }
    #[test]
    fn resolver_keeps_exact_original_keys_for_encoded_absolute_links() {
        let alias = alias();
        let original = "file:///Users/%C3%A5/project/my%20plot%23x.png#caption";
        let mut body = serde_json::json!({"base":"/Users/å/project","targets":[original,"relative%20plot.png","https://example.invalid/image.png"]});
        let keys = alias.request_body(&mut body);
        assert_eq!(body["base"], "/cloud/project");
        assert_eq!(body["targets"][0], "/cloud/project/my%20plot%23x.png");
        assert_eq!(body["targets"][1], "relative%20plot.png");
        let mut response = serde_json::json!({"results":{body["targets"][0].as_str().unwrap():{"path":"/cloud/project/my plot#x.png","ticket":"opaque"}}});
        Alias::restore_keys(&mut response, &keys);
        alias.response("/fs/resolve_targets", &mut response);
        assert_eq!(
            response["results"][original]["path"],
            "/Users/å/project/my plot#x.png"
        );
        assert_eq!(response["results"][original]["ticket"], "opaque");
    }
    #[test]
    fn alias_rejects_encoded_traversal_controls_and_noncanonical_encoding() {
        for raw in ["relative", "/root/../private", "/root/\nprivate"] {
            assert!(Alias::decode(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw),
                PathBuf::from("/project")
            )
            .is_err());
        }
        assert!(Alias::decode("L3Jvb3Q=", PathBuf::from("/project")).is_err());
    }
}
