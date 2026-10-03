//! Live host UI traffic. Render trees and closure handles belong to a running
//! agent and an attached client, never to its durable conversation journal.

use crate::driver::DriverStep;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

pub const UI_QUEUE: usize = 16;
pub const UI_EVENT_QUEUE: usize = 4;
pub const UI_REQUEST_BYTES: usize = 128 * 1024;
pub const UI_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
pub const UI_PENDING: usize = 32;
const UI_MODULE_BYTES: usize = 9 * 1024 * 1024;
const UI_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub struct NativeUiCommand {
    pub client_id: String,
    pub request_id: String,
    pub request: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(client: &str, method: &str) -> NativeUiCommand {
        NativeUiCommand::new(client, "browser-1", json!({"subtype":method})).unwrap()
    }

    #[test]
    fn transport_stamps_identity_and_restricts_control_methods() {
        let c = NativeUiCommand::new("ours", "one", json!({"subtype":"ui_attach","client_id":"other", "surface":"mobile", "answers":["ui_copy","can_use_tool"]})).unwrap();
        assert_eq!(c.request["client_id"], "ours");
        assert_eq!(c.request["surface"], "desktop");
        assert_eq!(c.request["answers"], json!(["ui_copy"]));
        assert!(
            NativeUiCommand::new("ours", "one", json!({"subtype":"set_permission_mode"})).is_err()
        );
        assert!(NativeUiCommand::new(
            "ours",
            "one",
            json!({"subtype":"ui_render","props":{"text":"x".repeat(UI_REQUEST_BYTES)}})
        )
        .is_err());
    }

    #[test]
    fn transient_responses_are_client_scoped_and_never_journaled() {
        let mut bridge = ClaudeUi::default();
        let attached = bridge.command(command("first", "ui_attach"));
        assert!(attached.events.is_empty());
        let id = &attached.outbound[0]["request_id"];
        let mut step = DriverStep::default();
        assert!(bridge.response(&json!({"response":{"request_id":id,"subtype":"success","response":{"surfaces":["desktop"]}}}), &mut step));
        assert!(step.events.is_empty());
        assert!(step
            .native_ui
            .iter()
            .all(|e| e.for_client("first") && !e.for_client("second")));
        let detached = bridge.command(command("first", "ui_detach"));
        assert_eq!(detached.outbound[0]["request"]["subtype"], "ui_detach");
        assert!(bridge
            .command(command("first", "ui_press"))
            .outbound
            .is_empty());
    }

    #[test]
    fn host_callbacks_cannot_be_answered_from_a_different_window() {
        let mut bridge = ClaudeUi::default();
        bridge.command(command("first", "ui_attach"));
        bridge.command(command("second", "ui_attach"));
        let request = json!({"type":"control_request","request_id":"native-copy","request":{"subtype":"ui_copy","client_id":"first","text":"copy me"}});
        let mut step = DriverStep::default();
        assert!(bridge.host_request(&request, &mut step));
        assert!(!step.native_ui[0].for_client("second"));
        let answer = |client| {
            NativeUiCommand::new(client, "answer", json!({"subtype":"ui_host_response","request_id":"native-copy","response":{"copied":true}})).unwrap()
        };
        assert!(bridge.command(answer("second")).outbound.is_empty());
        assert_eq!(
            bridge.command(answer("first")).outbound[0]["response"]["response"],
            json!({"copied":true})
        );
        assert!(bridge.command(answer("first")).outbound.is_empty());
    }

    #[test]
    fn pending_requests_expire_without_replaying_side_effects() {
        let mut bridge = ClaudeUi::default();
        bridge.command(command("first", "ui_attach"));
        let press = bridge.command(command("first", "ui_press"));
        let id = press.outbound[0]["request_id"].as_str().unwrap();
        bridge.pending.get_mut(id).unwrap().1 = Instant::now() - UI_TIMEOUT;
        let mut step = DriverStep::default();
        bridge.tick(&mut step);
        assert!(step.events.is_empty() && step.outbound.is_empty());
        assert_eq!(step.native_ui.len(), 1);
        assert!(!bridge.pending.contains_key(id));
        assert!(bridge.response(
            &json!({"response":{"request_id":id,"subtype":"success","response":{"handled":true}}}),
            &mut step
        ));
        assert_eq!(step.native_ui.len(), 1);
    }

    #[test]
    fn detach_is_delivered_even_when_another_window_fills_the_pending_budget() {
        let mut bridge = ClaudeUi::default();
        bridge.attached.extend(["leaving".into(), "busy".into()]);
        for _ in 0..UI_PENDING {
            bridge.command(command("busy", "ui_render"));
        }
        assert_eq!(bridge.pending.len(), UI_PENDING);
        let step = bridge.command(command("leaving", "ui_detach"));
        assert_eq!(step.outbound[0]["request"]["subtype"], "ui_detach");
        assert!(!bridge.attached.contains("leaving"));
        assert_eq!(bridge.pending.len(), UI_PENDING);
    }

    #[test]
    fn status_snapshot_retains_bounded_fields_not_unknown_payloads() {
        let mut bridge = ClaudeUi::default();
        let mut step = DriverStep::default();
        bridge.notification(&json!({"type":"system","subtype":"ui_status","plugin":"one","text":"Ready","unknown":"x".repeat(1024 * 1024)}), &mut step);
        let attached = bridge.command(command("first", "ui_attach"));
        step.native_ui.clear();
        bridge.response(&json!({"response":{"request_id":attached.outbound[0]["request_id"],"subtype":"success","response":{}}}), &mut step);
        assert!(serde_json::to_vec(&step.native_ui).unwrap().len() < 1024);
    }
}

impl NativeUiCommand {
    /// The authenticated transport assigns client_id. Never accept a browser's
    /// choice of another window's identity or arbitrary Claude control methods.
    pub fn new(client_id: &str, request_id: &str, mut request: Value) -> anyhow::Result<Self> {
        anyhow::ensure!(
            request_id.len() <= 64 && !request_id.is_empty(),
            "invalid UI request id"
        );
        anyhow::ensure!(
            serde_json::to_vec(&request)?.len() <= UI_REQUEST_BYTES,
            "UI request too large"
        );
        let method = request["subtype"].as_str().unwrap_or_default().to_owned();
        anyhow::ensure!(
            matches!(
                method.as_str(),
                "ui_attach"
                    | "ui_detach"
                    | "ui_render"
                    | "ui_press"
                    | "ui_input"
                    | "ui_select"
                    | "ui_panes"
                    | "ui_pane_show"
                    | "ui_pane_focus"
                    | "ui_close"
                    | "ui_scroll"
                    | "ui_focus"
                    | "ui_client_module"
                    | "ui_client_press"
                    | "ui_message"
                    | "ui_prompt_edit"
                    | "ui_host_response"
            ),
            "unsupported native UI request"
        );
        let request = request
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("UI request must be an object"))?;
        request.insert("client_id".into(), json!(client_id));
        request.insert("surface".into(), json!("desktop"));
        if method == "ui_attach" {
            let answers: Vec<Value> = request
                .get("answers")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|v| v.as_str().is_some_and(is_host_method))
                .take(4)
                .cloned()
                .collect();
            request.insert("answers".into(), json!(answers));
        }
        Ok(Self {
            client_id: client_id.into(),
            request_id: request_id.into(),
            request: Value::Object(request.clone()),
        })
    }

    pub fn failed(&self, message: &str) -> NativeUiEvent {
        NativeUiEvent::Response {
            client_id: self.client_id.clone(),
            request_id: self.request_id.clone(),
            result: None,
            error: Some(message.into()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NativeUiEvent {
    Response {
        client_id: String,
        request_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Notification {
        payload: Value,
    },
}

impl NativeUiEvent {
    pub fn for_client(&self, id: &str) -> bool {
        match self {
            Self::Response { client_id, .. } => client_id == id,
            Self::Notification { payload } => payload
                .get("client_id")
                .or_else(|| payload.get("request").and_then(|r| r.get("client_id")))
                .and_then(Value::as_str)
                .is_none_or(|v| v == id),
        }
    }
}

/// This protocol is live process state. Neither native closures nor modules
/// survive restart, and replaying a press could run an action twice.
#[derive(Default)]
pub(crate) struct ClaudeUi {
    next: u64,
    pending: HashMap<String, (NativeUiCommand, Instant)>,
    attached: HashSet<String>,
    statuses: HashMap<String, Value>,
    host_requests: HashMap<String, (String, String, Instant)>,
}

fn is_host_method(method: &str) -> bool {
    matches!(
        method,
        "ui_copy" | "ui_prompt_read" | "ui_prompt_fill" | "ui_prompt_suggest"
    )
}

impl ClaudeUi {
    pub fn command(&mut self, command: NativeUiCommand) -> DriverStep {
        let mut step = DriverStep::default();
        let method = command.request["subtype"].as_str().unwrap_or_default();
        if method == "ui_host_response" {
            return self.host_response(command);
        }
        if method == "ui_detach" {
            self.attached.remove(&command.client_id);
            self.pending
                .retain(|_, (c, _)| c.client_id != command.client_id);
            self.host_requests
                .retain(|_, (client, _, _)| client != &command.client_id);
            // Cleanup cannot compete with another window's pending renders.
            // Acknowledge delivery only; the caller is leaving and does not
            // need a native reply retained after its socket disappears.
            self.next = self.next.wrapping_add(1);
            step.outbound.push(json!({"type":"control_request", "request_id":format!("chimaera-ui-{}", self.next), "request":command.request}));
            step.native_ui.push(NativeUiEvent::Response {
                client_id: command.client_id,
                request_id: command.request_id,
                result: Some(json!({"requested":true})),
                error: None,
            });
            return step;
        } else if method == "ui_attach" {
            if self.attached.len() >= UI_PENDING && !self.attached.contains(&command.client_id) {
                step.native_ui
                    .push(command.failed("Too many attached Mod windows"));
                return step;
            }
            // A lagged/reconnected view discards old promises. Their late
            // replies must not consume its new queue or revive stale handles.
            self.pending
                .retain(|_, (c, _)| c.client_id != command.client_id);
        } else if !self.attached.contains(&command.client_id) {
            step.native_ui
                .push(command.failed("Attach the Mod window before using its controls"));
            return step;
        }
        if self.pending.len() >= UI_PENDING {
            step.native_ui
                .push(command.failed("Too many pending Mod requests"));
            return step;
        }
        if method == "ui_attach" {
            self.attached.insert(command.client_id.clone());
        }
        self.next = self.next.wrapping_add(1);
        let id = format!("chimaera-ui-{}", self.next);
        step.outbound
            .push(json!({"type":"control_request","request_id":id,"request":command.request}));
        self.pending.insert(id, (command, Instant::now()));
        step
    }

    pub fn response(&mut self, frame: &Value, step: &mut DriverStep) -> bool {
        let Some(id) = frame["response"]["request_id"].as_str() else {
            return false;
        };
        if !id.starts_with("chimaera-ui-") {
            return false;
        }
        let Some((command, _)) = self.pending.remove(id) else {
            return true;
        };
        let response = &frame["response"];
        let budget = if command.request["subtype"] == "ui_client_module" {
            UI_MODULE_BYTES
        } else {
            UI_RESPONSE_BYTES
        };
        if response["subtype"] != "success" {
            if command.request["subtype"] == "ui_attach" {
                self.attached.remove(&command.client_id);
            }
            let error = response["error"]
                .as_str()
                .unwrap_or("Claude rejected the Mod request");
            step.native_ui
                .push(command.failed(&error.chars().take(4096).collect::<String>()));
        } else if serde_json::to_vec(response).is_ok_and(|v| v.len() > budget) {
            step.native_ui
                .push(command.failed("Mod response exceeds the host's size limit"));
        } else {
            step.native_ui.push(NativeUiEvent::Response {
                client_id: command.client_id.clone(),
                request_id: command.request_id,
                result: Some(response["response"].clone()),
                error: None,
            });
            if command.request["subtype"] == "ui_attach" {
                step.native_ui.push(NativeUiEvent::Notification { payload: json!({
                    "type":"system", "subtype":"ui_status_snapshot", "client_id":command.client_id,
                    "statuses":self.statuses.values().collect::<Vec<_>>()
                }) });
            }
        }
        true
    }

    pub fn notification(&mut self, frame: &Value, step: &mut DriverStep) -> bool {
        let kind = frame["subtype"].as_str().unwrap_or_default();
        if !matches!(
            kind,
            "ui_invalidate" | "ui_panes" | "ui_focus" | "ui_scroll" | "ui_status" | "ui_toast"
        ) {
            return false;
        }
        if serde_json::to_vec(frame).is_ok_and(|v| v.len() <= UI_RESPONSE_BYTES) {
            if kind == "ui_status" {
                if let Some(plugin) = frame["plugin"].as_str() {
                    if frame["text"].is_null() {
                        self.statuses.remove(plugin);
                    } else if plugin.len() <= 256
                        && frame["text"].as_str().is_some_and(|t| t.len() <= 16 * 1024)
                        && (self.statuses.len() < 64 || self.statuses.contains_key(plugin))
                    {
                        // Retain only the bounded status fields. Unknown wire
                        // fields must not inflate a later aggregate snapshot.
                        self.statuses.insert(plugin.to_owned(), json!({"type":"system", "subtype":"ui_status", "plugin":plugin, "text":frame["text"]}));
                    }
                }
            }
            step.native_ui.push(NativeUiEvent::Notification {
                payload: frame.clone(),
            });
        }
        true
    }

    pub fn tick(&mut self, step: &mut DriverStep) {
        self.host_requests
            .retain(|_, (_, _, started)| started.elapsed() < Duration::from_secs(5));
        self.pending.retain(|_, (command, started)| {
            if started.elapsed() < UI_TIMEOUT {
                return true;
            }
            step.native_ui
                .push(command.failed("Claude did not answer the Mod request in time"));
            false
        });
    }

    pub fn host_request(&mut self, frame: &Value, step: &mut DriverStep) -> bool {
        let method = frame["request"]["subtype"].as_str().unwrap_or_default();
        if !is_host_method(method) {
            return false;
        }
        let Some(client) = frame["request"]["client_id"].as_str() else {
            return true;
        };
        let Some(id) = frame["request_id"].as_str() else {
            return true;
        };
        if self.attached.contains(client)
            && self.host_requests.len() < UI_PENDING
            && id.len() <= 256
            && serde_json::to_vec(frame).is_ok_and(|v| v.len() <= UI_RESPONSE_BYTES)
        {
            self.host_requests
                .insert(id.into(), (client.into(), method.into(), Instant::now()));
            step.native_ui.push(NativeUiEvent::Notification {
                payload: frame.clone(),
            });
        }
        true
    }

    fn host_response(&mut self, command: NativeUiCommand) -> DriverStep {
        let mut step = DriverStep::default();
        let id = command.request["request_id"].as_str().unwrap_or_default();
        let Some((client, method, started)) = self.host_requests.get(id) else {
            step.native_ui
                .push(command.failed("Mod host request is no longer pending"));
            return step;
        };
        let response = &command.request["response"];
        let valid = match method.as_str() {
            "ui_copy" => response["copied"].is_boolean(),
            "ui_prompt_fill" => response["filled"].is_boolean(),
            "ui_prompt_suggest" => response["shown"].is_boolean(),
            "ui_prompt_read" => response["text"].as_str().is_some_and(|text| {
                response["cursor"]
                    .as_u64()
                    .is_some_and(|cursor| cursor <= text.encode_utf16().count() as u64)
            }),
            _ => false,
        };
        if client != &command.client_id || started.elapsed() >= Duration::from_secs(5) || !valid {
            step.native_ui.push(
                command.failed("Mod host response does not match this window's pending request"),
            );
            return step;
        }
        self.host_requests.remove(id);
        step.outbound.push(json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":response}}));
        step.native_ui.push(NativeUiEvent::Response {
            client_id: command.client_id,
            request_id: command.request_id,
            result: Some(json!({"answered":true})),
            error: None,
        });
        step
    }
}
