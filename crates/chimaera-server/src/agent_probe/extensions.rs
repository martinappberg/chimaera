//! Native extension inventories. Only whitelisted presentation fields leave
//! this module: inspect output can also contain credentials and tool endpoints.
use super::*;

const ROW_LIMIT: usize = 512;

fn rows<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>, &'static str> {
    value[key]
        .as_array()
        .filter(|v| v.len() <= ROW_LIMIT)
        .ok_or("The agent returned an unrecognized extension inventory.")
}

fn text(value: &Value, key: &str) -> Option<String> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 4096 && !s.chars().any(char::is_control))
        .map(str::to_string)
}

fn skill(value: &Value, kind: AgentKind, root: &Path) -> Option<Value> {
    let name = text(value, "name")?;
    let path = if kind == AgentKind::Grok {
        text(&value["source"], "path")
    } else {
        text(value, "path")
    };
    let plugin = text(value, "plugin")
        .or_else(|| text(&value["source"], "plugin"))
        .or_else(|| text(&value["source"], "plugin_name"));
    let source = if plugin.is_some() || value["source"]["type"] == "plugin" {
        "plugin"
    } else if value["builtin"] == true || value["source"]["type"] == "bundled" {
        "builtin"
    } else if path
        .as_ref()
        .is_some_and(|p| Path::new(p).starts_with(root))
    {
        "project"
    } else {
        "user"
    };
    let invocable = value["userInvocable"].as_bool().unwrap_or(true);
    Some(
        json!({"name":name,"description":crate::timeline::cap(value["description"].as_str().unwrap_or(""),400),
        "source":source,"scope":if path.as_ref().is_some_and(|p| Path::new(p).starts_with(root)) {"project"} else {"user"},"plugin":plugin,"path":path,
        "invoke": if invocable { Some(format!("/{name}")) } else { None },
        "reason": if invocable { None } else { Some("This skill has no user command. Its visibility is controlled by the agent.") }}),
    )
}

fn grok_inventory(raw: &Value, root: &Path) -> Result<Value, &'static str> {
    let plugins: Vec<Value> = rows(raw,"plugins")?.iter().filter_map(|p| {
        let id = text(p,"name")?;
        Some(json!({"id":id,"enabled":p["enabled"].as_bool(),"scope":text(p,"scope"),
            "skills_n":p["provides"]["skills"].as_u64(),"has_hooks":p["provides"]["hooks"].as_bool(),
            "actions": if p["scope"] == "project" || p["path"].as_str().is_some_and(|p| p.contains("/.claude/")) { vec![] } else { vec!["enable_plugin","disable_plugin","update_plugin"] },
            "origin": if p["path"].as_str().is_some_and(|p| p.contains("/.claude/")) { "Claude compatibility" } else { "Grok" }}))
    }).collect();
    let skills: Vec<Value> = rows(raw, "skills")?
        .iter()
        .filter_map(|s| skill(s, AgentKind::Grok, root))
        .collect();
    let connections: Vec<Value> = rows(raw,"mcpServers")?.iter().filter_map(|c| {
        Some(json!({"name":text(c,"name")?,"kind":"mcp","status":if c["compatibilityStatus"] == "disabled" { "disabled" } else { "configured" },
            "source":if c["vendor"] == "claude" {"Claude compatibility"} else {"MCP server"},"login":false}))
    }).collect();
    Ok(
        json!({"plugins":plugins,"skills":skills,"connections":connections,"notice": if raw["projectTrusted"] == false {Some("Open Grok in this workspace to review project trust. Project extensions stay off until you trust the folder.")} else {None}}),
    )
}

// Only known native-command versions may run a slash command during discovery.
// An older CLI may otherwise send it to the model and bill a turn.
fn supports_agy_skill_inventory(version: Option<&str>) -> bool {
    let Some(version) = version else { return false };
    let Some(token) = version.split_whitespace().find(|s| {
        s.trim_start_matches('v')
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit())
    }) else {
        return false;
    };
    let parts: Vec<u32> = token
        .trim_start_matches('v')
        .split('.')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .unwrap_or_default();
    parts.len() == 3 && parts.as_slice() >= [1, 2, 14].as_slice()
}

fn agy_skills(output: &str, root: &Path) -> Result<Vec<Value>, &'static str> {
    let raw: Value = serde_json::from_str(output)
        .map_err(|_| "Antigravity couldn't report its loaded skills.")?;
    // Never mistake model prose for an inventory (older versions may send an
    // unknown slash command to the model). Only the native zero-turn command.
    if raw["command"]["name"] != "skills" || raw["num_turns"] != 0 || raw["status"] != "SUCCESS" {
        return Err("This Antigravity version couldn't list skills without a model turn.");
    }
    Ok(rows(&raw["command"]["data"], "skills")?
        .iter()
        .filter_map(|s| skill(s, AgentKind::Antigravity, root))
        .collect())
}

fn agy_plugins(output: &str, skills: &[Value]) -> Result<Vec<Value>, &'static str> {
    let imports = if output.trim() == "No imported plugins." {
        Vec::new()
    } else {
        let raw: Value = serde_json::from_str(output)
            .map_err(|_| "Antigravity couldn't report its installed plugins.")?;
        rows(&raw, "imports")?.clone()
    };
    let mut plugins = BTreeMap::new();
    for p in imports {
        let Some(id) = text(&p, "name") else { continue };
        // The import manifest has no enablement field. Installed is not proof
        // of enabled; a loaded skill below can positively confirm it.
        plugins.insert(("user".to_string(), id.clone()), json!({"id":id,"scope":"user","enabled":null,"origin":"Antigravity","actions":["enable_plugin","disable_plugin"]}));
    }
    for s in skills {
        let Some(id) = text(s, "plugin") else {
            continue;
        };
        let scope = text(s, "scope").unwrap_or_else(|| "user".into());
        let row = plugins
            .entry((scope.clone(), id.clone()))
            .or_insert_with(|| json!({"id":id,"scope":scope,"origin":"Antigravity"}));
        row["enabled"] = json!(true);
        row["skills_n"] = json!(row["skills_n"].as_u64().unwrap_or(0) + 1);
    }
    Ok(plugins.into_values().collect())
}

/// Shared result cached by workspace; Grok's inspect includes compatible
/// plugins that `plugin list` omits. Antigravity's skills command is read-only.
pub(super) async fn inventory(
    state: &Arc<AppState>,
    ws: &str,
    root: &Path,
    kind: AgentKind,
) -> Value {
    let agent = kind.as_str();
    let key = format!("extensions:{agent}:{ws}");
    if let Some(hit) = state.probes.get(&key) {
        return hit;
    }
    let work = async {
        let _permit = GATE.acquire().await;
        if let Some(hit) = state.probes.get(&key) {
            return hit;
        }
        let (bin, version, _usage) = match bin_of(state, kind).await {
            Ok(v) => v,
            Err(_) => {
                return json!({"agent":agent,"available":false,"plugins":[],"skills":[],"connections":[]})
            }
        };
        let generation = state.probes.generation.load(Ordering::Relaxed);
        let prelude = ProbePrelude::write(state, Some(ws)).await;
        let mut result = json!({"agent":agent,"available":true,"version":version,"plugins":[],"skills":[],"connections":[],"actions":["manage_plugins","manage_connections","install_plugin"]});
        match kind {
            AgentKind::Grok => {
                let raw = connections::run_cli(&bin, &["inspect", "--json"], root, prelude.path())
                    .await
                    .map_err(|_| "Grok couldn't report its extensions.")
                    .and_then(|s| {
                        serde_json::from_str::<Value>(&s)
                            .map_err(|_| "Grok returned an unrecognized inventory.")
                    })
                    .and_then(|v| grok_inventory(&v, root));
                match raw {
                    Ok(v) => {
                        for key in ["plugins", "skills", "connections", "notice"] {
                            result[key] = v[key].clone();
                        }
                    }
                    Err(e) => result["error"] = json!(e),
                }
            }
            AgentKind::Antigravity => {
                let skills = if supports_agy_skill_inventory(version.as_deref()) {
                    connections::run_cli(
                    &bin,
                    &[
                        "-p",
                        "/skills",
                        "--output-format",
                        "json",
                        "--print-timeout",
                        "15s",
                    ],
                    root,
                    prelude.path(),
                )
                .await
                .map_err(|_| {
                    "Antigravity couldn't list its loaded skills. Open Antigravity to check setup."
                })
                .and_then(|out| agy_skills(&out, root))
                } else if version.is_none() {
                    Err("Couldn't check Antigravity's version. Try again.")
                } else {
                    Err("Update Antigravity to list its skills here.")
                };
                match skills {
                    Ok(v) => result["skills"] = json!(v),
                    Err(e) => result["skills_error"] = json!(e),
                }
                let listed = connections::run_cli(&bin, &["plugin", "list"], root, prelude.path())
                    .await
                    .map_err(|_| "Antigravity couldn't report its installed plugins.")
                    .and_then(|out| agy_plugins(&out, result["skills"].as_array().unwrap()));
                match listed {
                    Ok(v) => result["plugins"] = json!(v),
                    Err(e) => result["error"] = json!(e),
                }
                result["notice"]=json!("Antigravity reports installed packages and plugins with loaded skills. Other project plugins are managed in Antigravity.");
            }
            _ => result["error"] = json!("Extension discovery is unavailable for this agent."),
        }
        state.probes.put(&key, result.clone(), generation);
        result
    };
    tokio::time::timeout(Duration::from_secs(60),work).await.unwrap_or_else(|_| json!({"agent":agent,"available":true,"plugins":[],"skills":[],"connections":[],"error":"Checking extensions timed out. Try again."}))
}

pub(super) fn append_skills(skills: &mut Vec<Value>, inventory: &Value) {
    let Some(agent) = inventory["agent"].as_str() else {
        return;
    };
    for skill in inventory["skills"].as_array().into_iter().flatten() {
        let Some(name) = skill["name"].as_str() else {
            continue;
        };
        // Equal names are not necessarily equal skills: keep different source
        // files separate so their descriptions and provider provenance survive.
        let existing = skills.iter().position(|s| {
            s["name"] == name
                && s["paths"].as_object().is_some_and(|paths| {
                    paths.values().any(|p| !p.is_null() && p == &skill["path"])
                })
        });
        let i = existing.unwrap_or_else(|| {
            skills.push(json!({"name":name,"description":skill["description"],"source":skill["source"],"plugin":skill["plugin"],"paths":{},"agents":{}}));
            skills.len()-1
        });
        skills[i]["paths"][agent] = skill["path"].clone();
        skills[i]["agents"][agent] =
            json!({"state":"available","invoke":skill["invoke"],"reason":skill["reason"]});
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_and_old_agy_versions_never_probe_a_prompt() {
        for version in [
            None,
            Some("unknown"),
            Some("1.2.13"),
            Some("1.2.14-preview"),
        ] {
            assert!(!supports_agy_skill_inventory(version));
        }
        assert!(supports_agy_skill_inventory(Some("1.2.14")));
        assert!(supports_agy_skill_inventory(Some("agy v1.3.0")));
    }
    #[test]
    fn grok_preserves_effective_inventory_without_leaking_config() {
        let raw = json!({"plugins":[{"name":"kit","path":"/home/me/.claude/plugins/kit","enabled":false,"scope":"user","provides":{"skills":2,"hooks":true}}],"skills":[{"name":"review","source":{"type":"user","path":"/home/me/skills/review/SKILL.md"},"userInvocable":false}],"mcpServers":[{"name":"private","target":"https://secret.example/?token=secret","vendor":"claude"}],"credentials":"secret"});
        let got = grok_inventory(&raw, Path::new("/work")).unwrap();
        assert_eq!(got["plugins"][0]["origin"], "Claude compatibility");
        assert_eq!(got["plugins"][0]["enabled"], false);
        assert!(got["skills"][0]["invoke"].is_null());
        assert!(!got.to_string().contains("secret"));
    }
    #[test]
    fn antigravity_only_accepts_native_inventory_and_positive_enablement() {
        assert!(agy_skills(
            r#"{"status":"SUCCESS","num_turns":1,"response":"skills"}"#,
            Path::new("/work")
        )
        .is_err());
        let got = agy_plugins(r#"{"imports":[{"name":"kit"}]}"#, &[]).unwrap();
        assert!(got[0]["enabled"].is_null());
        let got = agy_plugins(
            "No imported plugins.",
            &[json!({"plugin":"project-kit","source":"project"})],
        )
        .unwrap();
        assert_eq!(got[0]["enabled"], true);
    }
    #[test]
    fn same_name_from_different_agents_keeps_its_own_definition() {
        let mut out = vec![
            json!({"name":"review","description":"Claude review","paths":{"claude":"/claude/SKILL.md"},"agents":{"claude":{"state":"available"}}}),
        ];
        append_skills(
            &mut out,
            &json!({"agent":"grok","skills":[{"name":"review","description":"Grok review","path":"/grok/SKILL.md","invoke":"/review"}]}),
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["description"], "Claude review");
        assert_eq!(out[1]["agents"]["grok"]["invoke"], "/review");
    }
}
