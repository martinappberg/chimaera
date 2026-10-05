//! ACP v1 over stdio. Protocol translation stays here; executable discovery,
//! installation and argv belong to the daemon's harness registry/launcher.
use crate::capabilities::ChatCapabilities;
use crate::driver::{
    run_driver, AgentAdapter, Driver, DriverExit, DriverIo, DriverStep, Handshake, Mapper,
    SpawnSpec,
};
use crate::model::*;
use crate::ndjson::{JsonlSink, JsonlStream};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use tokio::task::JoinHandle;

pub const TESTED_GROK_VERSION: &str = "1.0.46";
pub const TESTED_AGY_VERSION: &str = "1.2.1";
const MAX_PENDING: usize = 64;
const MAX_CHOICES: usize = 64;

/// A distribution supplies identity/auth policy; the protocol engine is shared.
#[derive(Clone, Copy)]
pub struct AcpAdapter {
    pub kind: &'static str,
    pub tested_version: &'static str,
    pub auth_method: &'static str,
}
pub const GROK: AcpAdapter = AcpAdapter {
    kind: "grok",
    tested_version: TESTED_GROK_VERSION,
    auth_method: "cached_token",
};
pub const ANTIGRAVITY: AcpAdapter = AcpAdapter {
    kind: "agy",
    tested_version: TESTED_AGY_VERSION,
    auth_method: "oauth-personal",
};

impl AgentAdapter for AcpAdapter {
    fn kind(&self) -> &'static str {
        self.kind
    }
    fn spawn(&self, spec: SpawnSpec, io: DriverIo) -> anyhow::Result<JoinHandle<DriverExit>> {
        anyhow::ensure!(!spec.argv.is_empty(), "empty argv");
        let driver = *self;
        Ok(tokio::spawn(run_driver(driver, spec, io)))
    }
}

async fn rpc(
    sink: &mut JsonlSink,
    stream: &mut JsonlStream,
    id: u64,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    sink.send(&json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}))
        .await
        .map_err(|e| e.to_string())?;
    loop {
        let frame = stream
            .next()
            .await
            .map_err(|e| e.to_string())?
            .ok_or("ACP exited during initialization")?;
        if frame.get("id") == Some(&json!(id)) && frame.get("method").is_none() {
            if let Some(error) = frame.get("error") {
                return Err(
                    cap_output(error["message"].as_str().unwrap_or("ACP request failed")).0,
                );
            }
            return Ok(frame["result"].clone());
        }
        // Clients advertise no filesystem/terminal callbacks. Refuse unknown
        // requests explicitly so an agent can never hang waiting for us.
        if frame.get("method").is_some() && frame.get("id").is_some() {
            sink.send(&json!({"jsonrpc":"2.0", "id":frame["id"], "error":{"code":-32601,"message":"Client method not supported during initialization"}})).await.map_err(|e| e.to_string())?;
        }
    }
}
impl Driver for AcpAdapter {
    type Mapper = AcpMapper;
    fn kind(&self) -> &'static str {
        self.kind
    }
    fn tested_version(&self) -> &'static str {
        self.tested_version
    }
    async fn handshake(
        &self,
        sink: &mut JsonlSink,
        stream: &mut JsonlStream,
        spec: &SpawnSpec,
        progress: &tokio::sync::mpsc::Sender<AgentEvent>,
    ) -> Result<Handshake<AcpMapper>, String> {
        if spec.fork_at.is_some() || spec.rollback_turns.is_some() {
            return Err("This ACP adapter requires a conversation-copy fork".into());
        }
        let init = rpc(sink, stream, 1, "initialize", json!({"protocolVersion":1,"clientInfo":{"name":"chimaera","version":env!("CARGO_PKG_VERSION")},"clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false}})).await?;
        if init["protocolVersion"] != 1 {
            return Err("ACP protocol version 1 required".into());
        }
        crate::driver::startup_progress(progress, "Checking agent sign-in…").await;
        rpc(
            sink,
            stream,
            2,
            "authenticate",
            json!({"methodId":self.auth_method,"_meta":{"headless":true}}),
        )
        .await?;
        let mut params = json!({"cwd":spec.cwd,"mcpServers":spec.mcp_servers});
        let method = if let Some(id) = &spec.pinned_native_id {
            if init["agentCapabilities"]["loadSession"] != true {
                return Err("Agent does not support resuming this chat".into());
            }
            params["sessionId"] = json!(id);
            // session/resume restores state without replaying vendor history;
            // Chimaera owns the durable transcript and replays it itself.
            if init["agentCapabilities"]["sessionCapabilities"]
                .get("resume")
                .is_some()
            {
                "session/resume"
            } else {
                "session/load"
            }
        } else {
            "session/new"
        };
        crate::driver::startup_progress(progress, "Opening conversation and loading tools…").await;
        let session = rpc(sink, stream, 3, method, params).await?;
        let native = session["sessionId"]
            .as_str()
            .or(spec.pinned_native_id.as_deref())
            .ok_or("ACP omitted sessionId")?;
        if native.is_empty() || native.len() > 512 {
            return Err("Invalid ACP sessionId".into());
        }
        let mut mapper = AcpMapper::new(native.to_owned(), &init, &session);
        // ACP has no portable system-context field. Hold the bounded handoff
        // until a real user send: opening a fork must stay idle and unbilled.
        mapper.portable_context = spec.portable_context.clone();
        let mut initial = vec![mapper.config_state(false)];
        // Settings use the same acknowledged protocol path as user changes.
        for command in [
            spec.initial_model.as_ref().map(|v| AgentCommand::SetModel {
                model_id: v.clone(),
            }),
            spec.initial_mode
                .as_ref()
                .map(|v| AgentCommand::SetMode { mode_id: v.clone() }),
            spec.initial_effort
                .as_ref()
                .map(|v| AgentCommand::SetEffort {
                    effort_id: v.clone(),
                }),
        ]
        .into_iter()
        .flatten()
        {
            initial.push(mapper.on_command(command));
        }
        Ok(Handshake { mapper, initial })
    }
}

struct Ask {
    wire_id: Value,
    options: Vec<PermissionOption>,
}
enum Pending {
    Prompt(String),
    Config(String, String),
    Mode(String),
    Model(String),
}
struct Queued {
    id: String,
    blocks: Vec<ContentBlock>,
}
pub struct AcpMapper {
    native: String,
    model: Option<String>,
    mode: Option<String>,
    modes: Vec<ModeInfo>,
    models: Vec<ModelInfo>,
    commands: Vec<SlashCommand>,
    config: Value,
    caps: ChatCapabilities,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    asks: HashMap<String, Ask>,
    queue: VecDeque<Queued>,
    turn: Option<String>,
    interrupted: bool,
    coalescer: Coalescer,
    portable_context: Option<String>,
}
fn text(v: &Value) -> String {
    cap_output(v.as_str().unwrap_or_default()).0
}
fn small(v: &Value) -> String {
    cap_head_tail(v.as_str().unwrap_or_default(), 128, 0).0
}
fn identifier(v: &Value) -> Option<String> {
    v.as_str()
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .map(str::to_owned)
}
fn path(v: &Value) -> Option<String> {
    // Truncating an identity/path changes what a later action targets.
    v.as_str()
        .filter(|s| !s.is_empty() && s.len() <= 1024)
        .map(str::to_owned)
}
fn rows(v: &Value) -> impl Iterator<Item = &Value> {
    v.as_array().into_iter().flatten().take(MAX_CHOICES)
}
fn unique_rows<'a>(v: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    let mut seen = HashSet::new();
    let key = key.to_owned();
    rows(v).filter(move |row| {
        row[&key]
            .as_str()
            .is_some_and(|id| !id.is_empty() && id.len() <= 128 && seen.insert(id.to_owned()))
    })
}

impl AcpMapper {
    fn new(native: String, init: &Value, session: &Value) -> Self {
        let models = unique_rows(&session["models"]["availableModels"], "modelId")
            .filter_map(|m| {
                let id = small(&m["modelId"]);
                (!id.is_empty()).then(|| ModelInfo {
                    label: m["name"]
                        .as_str()
                        .map(|s| cap_head_tail(s, 128, 0).0)
                        .unwrap_or_else(|| id.clone()),
                    id,
                    description: m["description"]
                        .as_str()
                        .map(|s| cap_head_tail(s, 256, 0).0),
                    resolved: None,
                    efforts: Vec::new(),
                    default_effort: None,
                })
            })
            .collect();
        let modes = unique_rows(&session["modes"]["availableModes"], "id")
            .filter_map(|m| {
                let id = small(&m["id"]);
                (!id.is_empty()).then(|| ModeInfo {
                    id,
                    label: small(&m["name"]),
                })
            })
            .collect();
        let mut mapper = Self {
            native,
            model: identifier(&session["models"]["currentModelId"]),
            mode: identifier(&session["modes"]["currentModeId"]),
            models,
            modes,
            commands: Vec::new(),
            config: Value::Null,
            caps: ChatCapabilities {
                commands: [
                    "send",
                    "permission",
                    "interrupt",
                    "send_after_turn",
                    "cancel_queued",
                    "send_now",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect(),
                image_input: init["agentCapabilities"]["promptCapabilities"]["image"] == true,
                custom_model: false,
            },
            next_id: 10,
            pending: HashMap::new(),
            asks: HashMap::new(),
            queue: VecDeque::new(),
            turn: None,
            interrupted: false,
            coalescer: Coalescer::new(),
            portable_context: None,
        };
        if !mapper.models.is_empty() {
            mapper.caps.commands.push("set_model".into());
        }
        if !mapper.modes.is_empty() {
            mapper.caps.commands.push("set_mode".into());
        }
        mapper.set_config(&session["configOptions"]);
        mapper
    }
    fn set_config(&mut self, config: &Value) {
        // Keep only bounded controls; descriptions and provider metadata are
        // untrusted and are not part of the executable configuration.
        self.config = Value::Array(unique_rows(config, "id").filter(|c| c["type"] == "select").map(|c| {
            let flattened = Value::Array(rows(&c["options"]).flat_map(|o| {
                if o["options"].is_array() { rows(&o["options"]).collect::<Vec<_>>() } else { vec![o] }
            }).take(MAX_CHOICES).cloned().collect());
            let options: Vec<_> = unique_rows(&flattened, "value")
            .map(|o| json!({"value":small(&o["value"]),"name":small(&o["name"])})).collect();
            json!({"id":small(&c["id"]),"category":small(&c["category"]),"currentValue":small(&c["currentValue"]),"options":options})
        }).collect());
        if let Some(c) = rows(&self.config).find(|c| c["category"] == "model") {
            self.models = rows(&c["options"])
                .map(|o| ModelInfo {
                    id: small(&o["value"]),
                    label: small(&o["name"]),
                    description: None,
                    resolved: None,
                    efforts: Vec::new(),
                    default_effort: None,
                })
                .collect();
            self.model = Some(small(&c["currentValue"]));
        }
        if let Some(c) = rows(&self.config).find(|c| c["category"] == "mode") {
            self.modes = rows(&c["options"])
                .map(|o| ModeInfo {
                    id: small(&o["value"]),
                    label: small(&o["name"]),
                })
                .collect();
            self.mode = Some(small(&c["currentValue"]));
        }
        for model in &mut self.models {
            model.efforts.clear();
            model.default_effort = None;
        }
        let effort = rows(&self.config).find(|c| c["category"] == "thought_level");
        if let Some(c) = effort {
            if let Some(model) = self
                .models
                .iter_mut()
                .find(|m| Some(&m.id) == self.model.as_ref())
            {
                model.efforts = rows(&c["options"]).map(|o| small(&o["value"])).collect();
                model.default_effort = Some(small(&c["currentValue"]));
            }
        }
        for (command, offered) in [
            ("set_model", !self.models.is_empty()),
            ("set_mode", !self.modes.is_empty()),
            ("set_effort", effort.is_some()),
        ] {
            self.caps.commands.retain(|c| c != command);
            if offered {
                self.caps.commands.push(command.into());
            }
        }
    }
    fn config_state(&self, chosen: bool) -> DriverStep {
        let mut step = DriverStep::default();
        step.events.push(self.catalog());
        step.events.push(AgentEvent::Capabilities {
            capabilities: self.caps.clone(),
        });
        step.events.push(AgentEvent::EffortState {
            effort: rows(&self.config)
                .find(|c| c["category"] == "thought_level")
                .map(|c| small(&c["currentValue"])),
            ultracode: false,
            chosen,
        });
        step
    }
    fn request(&mut self, method: &str, params: Value, pending: Pending, step: &mut DriverStep) {
        if self.pending.len() >= MAX_PENDING {
            step.events.push(AgentEvent::Error {
                message: "Too many pending agent requests".into(),
                fatal: false,
            });
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, pending);
        step.outbound
            .push(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
    }
    fn flush_into(&mut self, step: &mut DriverStep) {
        step.events.extend(self.coalescer.flush());
    }
    fn echo(blocks: &[ContentBlock], id: String, queued: bool, after_turn: bool) -> AgentEvent {
        AgentEvent::UserMessage {
            text: blocks_text(blocks),
            attachments: blocks
                .iter()
                .filter(|b| matches!(b, ContentBlock::Image { .. }))
                .count() as u32,
            attachment_paths: image_paths(blocks),
            id: Some(id),
            queued,
            after_turn,
            origin: None,
            client_id: None,
        }
    }
    fn start(&mut self, item: Queued, was_queued: bool, step: &mut DriverStep) {
        if self.pending.len() >= MAX_PENDING {
            if !was_queued {
                step.events
                    .push(Self::echo(&item.blocks, item.id.clone(), true, true));
            }
            step.events.push(AgentEvent::UserMessageUpdate {
                id: item.id,
                state: UserMessageState::Dropped,
            });
            step.events.push(AgentEvent::Error {
                message: "Too many pending agent requests; resend this message".into(),
                fatal: false,
            });
            return;
        }
        let mut prompt: Vec<Value> = item
            .blocks
            .iter()
            .map(|b| match b {
                ContentBlock::Text { text } => json!({"type":"text","text":text}),
                ContentBlock::Image {
                    media_type, data, ..
                } => json!({"type":"image","mimeType":media_type,"data":data}),
                ContentBlock::Skill { name, .. } => {
                    json!({"type":"text","text":format!("/{name}")})
                }
            })
            .collect();
        if let Some(context) = &self.portable_context {
            prompt.insert(0, json!({"type":"text","text":context}));
        }
        let turn = fresh_uuid();
        self.request(
            "session/prompt",
            json!({"sessionId":self.native,"prompt":prompt}),
            Pending::Prompt(turn.clone()),
            step,
        );
        if was_queued {
            step.events.push(AgentEvent::UserMessageUpdate {
                id: item.id,
                state: UserMessageState::Sent,
            });
        } else {
            step.events
                .push(Self::echo(&item.blocks, item.id, false, false));
        }
        self.turn = Some(turn.clone());
        self.interrupted = false;
        step.events.push(AgentEvent::TurnStarted { turn_id: turn });
    }
    fn cancel(&mut self, step: &mut DriverStep) {
        if self.turn.is_none() {
            return;
        }
        self.interrupted = true;
        for (request_id, ask) in self.asks.drain() {
            step.outbound.push(json!({"jsonrpc":"2.0","id":ask.wire_id,"result":{"outcome":{"outcome":"cancelled"}}}));
            step.events.push(AgentEvent::PermissionResolved {
                request_id,
                option_id: "cancelled".into(),
            });
        }
        step.outbound.push(
            json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":self.native}}),
        );
    }
    fn accept_context(&mut self, step: &mut DriverStep) {
        if self.turn.is_some() && self.portable_context.take().is_some() {
            step.events.push(AgentEvent::ForkContextConsumed);
        }
    }
    fn config_command(&mut self, category: &str, value: &str, step: &mut DriverStep) {
        let control = rows(&self.config)
            .find(|c| c["category"] == category)
            .cloned();
        if let Some(c) = control {
            if !rows(&c["options"]).any(|o| o["value"] == value) {
                step.events.push(AgentEvent::Notice {
                    text: "That option is not offered by this agent".into(),
                });
                return;
            }
            self.request(
                "session/set_config_option",
                json!({"sessionId":self.native,"configId":c["id"],"value":value}),
                Pending::Config(category.into(), value.into()),
                step,
            );
        } else {
            let (method, params, pending) = match category {
                "model" if self.models.iter().any(|m| m.id == value) => (
                    "session/set_model",
                    json!({"sessionId":self.native,"modelId":value}),
                    Pending::Model(value.into()),
                ),
                "mode" if self.modes.iter().any(|m| m.id == value) => (
                    "session/set_mode",
                    json!({"sessionId":self.native,"modeId":value}),
                    Pending::Mode(value.into()),
                ),
                _ => {
                    step.events.push(AgentEvent::Notice {
                        text: "This control is not offered by the agent".into(),
                    });
                    return;
                }
            };
            self.request(method, params, pending, step);
        }
    }
    fn catalog(&self) -> AgentEvent {
        AgentEvent::Catalog {
            models: self.models.clone(),
            modes: self.modes.clone(),
            slash_commands: self.commands.clone(),
        }
    }
    fn changed(&mut self, category: &str, value: String, chosen: bool, step: &mut DriverStep) {
        match category {
            "model" => {
                let from = self.model.replace(value.clone());
                step.events.push(AgentEvent::ModelSwitched {
                    from,
                    to: value,
                    reason: None,
                    retract_current_turn: false,
                });
            }
            "mode" => {
                self.mode = Some(value.clone());
                step.events.push(AgentEvent::ModeChanged {
                    mode_id: value,
                    chosen,
                });
            }
            "thought_level" => step.events.push(AgentEvent::EffortState {
                effort: Some(value),
                ultracode: false,
                chosen,
            }),
            _ => {}
        }
    }
}
impl Mapper for AcpMapper {
    fn capabilities(&self) -> Option<ChatCapabilities> {
        Some(self.caps.clone())
    }
    fn init_event(&self) -> AgentEvent {
        AgentEvent::Init {
            native_session_id: self.native.clone(),
            model: self.model.clone(),
            modes: self.modes.clone(),
            current_mode: self.mode.clone(),
            slash_commands: self.commands.clone(),
            models: self.models.clone(),
            agent_version: None,
            remote_control_available: false,
            remote_control_auto_enable: false,
            remote_control: None,
        }
    }
    fn flush(&mut self) -> Option<AgentEvent> {
        self.coalescer.flush()
    }
    fn on_command(&mut self, cmd: AgentCommand) -> DriverStep {
        let mut step = DriverStep::default();
        match cmd {
            AgentCommand::Send { blocks } | AgentCommand::SendAfterTurn { blocks } => {
                let id = fresh_uuid();
                if (!self.caps.image_input
                    && blocks
                        .iter()
                        .any(|b| matches!(b, ContentBlock::Image { .. })))
                    || blocks
                        .iter()
                        .any(|b| matches!(b, ContentBlock::Skill { .. }))
                    || self.pending.len() >= MAX_PENDING
                    || self.queue.len() >= 64
                {
                    step.events
                        .push(Self::echo(&blocks, id.clone(), true, true));
                    step.events.push(AgentEvent::UserMessageUpdate {
                        id,
                        state: UserMessageState::Dropped,
                    });
                    step.events.push(AgentEvent::Error {
                        message:
                            "The agent cannot accept this message's content or its queue is full"
                                .into(),
                        fatal: false,
                    });
                    return step;
                }
                if self.turn.is_some() {
                    step.events
                        .push(Self::echo(&blocks, id.clone(), true, true));
                    self.queue.push_back(Queued { id, blocks });
                } else {
                    self.start(Queued { id, blocks }, false, &mut step);
                }
            }
            AgentCommand::SendIfRunning { id, .. } => {
                step.events.push(AgentEvent::UserMessageUpdate {
                    id,
                    state: UserMessageState::Dropped,
                })
            }
            AgentCommand::CancelQueued { id } => {
                if let Some(index) = self.queue.iter().position(|q| q.id == id) {
                    self.queue.remove(index);
                    step.events.push(AgentEvent::UserMessageUpdate {
                        id,
                        state: UserMessageState::Cancelled,
                    });
                }
            }
            AgentCommand::SendNow { id } => {
                if let Some(index) = self.queue.iter().position(|q| q.id == id) {
                    let item = self.queue.remove(index).unwrap();
                    self.queue.push_front(item);
                    self.cancel(&mut step);
                }
            }
            AgentCommand::Interrupt => self.cancel(&mut step),
            AgentCommand::Permission {
                request_id,
                option_id,
                ..
            } => {
                if self
                    .asks
                    .get(&request_id)
                    .is_some_and(|a| a.options.iter().any(|o| o.id == option_id))
                {
                    let ask = self.asks.remove(&request_id).unwrap();
                    step.outbound.push(json!({"jsonrpc":"2.0","id":ask.wire_id,"result":{"outcome":{"outcome":"selected","optionId":option_id}}}));
                    step.events.push(AgentEvent::PermissionResolved {
                        request_id,
                        option_id,
                    });
                }
            }
            AgentCommand::SetModel { model_id } => {
                self.config_command("model", &model_id, &mut step)
            }
            AgentCommand::SetMode { mode_id } => self.config_command("mode", &mode_id, &mut step),
            AgentCommand::SetEffort { effort_id } => {
                self.config_command("thought_level", &effort_id, &mut step)
            }
            _ => step.events.push(AgentEvent::Notice {
                text: "This agent does not support that chat control".into(),
            }),
        }
        step
    }
    fn on_frame(&mut self, frame: &Value) -> DriverStep {
        let mut step = DriverStep::default();
        if frame.get("method").is_none() {
            let Some(pending) = frame["id"].as_u64().and_then(|id| self.pending.remove(&id)) else {
                return step;
            };
            let error = frame.get("error").map(|e| text(&e["message"]));
            match pending {
                Pending::Prompt(turn_id) => {
                    self.flush_into(&mut step);
                    if let Some(reason) = error {
                        step.events.push(AgentEvent::TurnAborted {
                            turn_id,
                            reason,
                            interrupted: self.interrupted,
                        });
                    } else if frame["result"]["stopReason"] == "cancelled" {
                        step.events.push(AgentEvent::TurnAborted {
                            turn_id,
                            reason: "stopped".into(),
                            interrupted: true,
                        });
                    } else {
                        self.accept_context(&mut step);
                        step.events.push(AgentEvent::TurnCompleted {
                            turn_id,
                            usage: Usage::default(),
                        });
                    }
                    self.turn = None;
                    for (request_id, _) in self.asks.drain() {
                        step.events.push(AgentEvent::PermissionResolved {
                            request_id,
                            option_id: "cancelled".into(),
                        });
                    }
                    if let Some(item) = self.queue.pop_front() {
                        self.start(item, true, &mut step);
                    }
                }
                _ if error.is_some() => step.events.push(AgentEvent::Error {
                    message: error.unwrap(),
                    fatal: false,
                }),
                Pending::Config(category, value) => {
                    let before_model = self.model.clone();
                    if frame["result"]["configOptions"].is_array() {
                        self.set_config(&frame["result"]["configOptions"]);
                    }
                    let value = rows(&self.config)
                        .find(|c| c["category"] == category)
                        .map(|c| small(&c["currentValue"]))
                        .unwrap_or(value);
                    if category == "model" {
                        self.model = before_model;
                    }
                    self.changed(&category, value, true, &mut step);
                    step.events.extend(self.config_state(false).events);
                }
                Pending::Model(value) => self.changed("model", value, true, &mut step),
                Pending::Mode(value) => self.changed("mode", value, true, &mut step),
            }
            return step;
        }
        let params = &frame["params"];
        if frame["method"] == "session/request_permission" && frame.get("id").is_some() {
            self.flush_into(&mut step);
            let request_id = frame["id"].to_string();
            if request_id.len() > 256 {
                return step;
            }
            let options: Vec<_> = unique_rows(&params["options"], "optionId")
                .take(8)
                .filter_map(|o| {
                    let kind = match o["kind"].as_str()? {
                        "allow_once" => PermissionOptionKind::AllowOnce,
                        "allow_always" => PermissionOptionKind::AllowAlways,
                        "reject_once" => PermissionOptionKind::RejectOnce,
                        "reject_always" => PermissionOptionKind::RejectAlways,
                        _ => return None,
                    };
                    Some(PermissionOption {
                        id: small(&o["optionId"]),
                        // Some providers reuse TUI labels promising a feedback
                        // editor. ACP's permission response has no such field.
                        label: match kind {
                            PermissionOptionKind::RejectOnce => "Deny".into(),
                            PermissionOptionKind::RejectAlways => "Always deny".into(),
                            _ => small(&o["name"]),
                        },
                        kind,
                    })
                })
                .collect();
            if params["sessionId"] != self.native
                || self.asks.len() >= MAX_PENDING
                || options.is_empty()
            {
                step.outbound.push(json!({"jsonrpc":"2.0","id":frame["id"],"result":{"outcome":{"outcome":"cancelled"}}}));
            } else {
                let call = &params["toolCall"];
                self.asks.insert(
                    request_id.clone(),
                    Ask {
                        wire_id: frame["id"].clone(),
                        options: options.clone(),
                    },
                );
                step.events.push(AgentEvent::PermissionRequest {
                    request_id,
                    tool_call_id: identifier(&call["toolCallId"]),
                    title: small(&call["title"]),
                    options,
                    input_preview: cap_preview(&call["rawInput"]),
                    plan: None,
                });
            }
            return step;
        }
        if frame.get("id").is_some() {
            step.outbound.push(json!({"jsonrpc":"2.0","id":frame["id"],"error":{"code":-32601,"message":"Client method not supported"}}));
            return step;
        }
        if frame["method"] != "session/update" || params["sessionId"] != self.native {
            return step;
        }
        let update = &params["update"];
        match update["sessionUpdate"].as_str().unwrap_or_default() {
            "agent_message_chunk" | "agent_thought_chunk" => {
                if update["content"]["type"] == "text" {
                    if update["content"]["text"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty())
                    {
                        self.accept_context(&mut step);
                    }
                    if let Some(turn) = &self.turn {
                        let kind = if update["sessionUpdate"] == "agent_thought_chunk" {
                            ChunkKind::Thought
                        } else {
                            ChunkKind::Message
                        };
                        step.events.extend(self.coalescer.push(
                            turn,
                            kind,
                            &text(&update["content"]["text"]),
                        ));
                    }
                }
            }
            "tool_call" | "tool_call_update" => {
                self.accept_context(&mut step);
                self.flush_into(&mut step);
                let Some(id) = identifier(&update["toolCallId"]) else {
                    return step;
                };
                let status = match update["status"].as_str() {
                    Some("completed") => ToolStatus::Completed,
                    Some("failed") => ToolStatus::Failed,
                    Some("pending") => ToolStatus::Pending,
                    _ => ToolStatus::InProgress,
                };
                if update["sessionUpdate"] == "tool_call" {
                    let kind =
                        serde_json::from_value(update["kind"].clone()).unwrap_or(ToolKind::Other);
                    step.events.push(AgentEvent::ToolCall {
                        id: id.clone(),
                        kind,
                        title: small(&update["title"]),
                        locations: rows(&update["locations"])
                            .take(16)
                            .filter_map(|v| path(&v["path"]))
                            .collect(),
                        status,
                        cross_turn: false,
                        command: None,
                    });
                }
                let content = rows(&update["content"])
                    .take(4)
                    .filter_map(|v| {
                        if v["type"] == "diff" {
                            let (new_text, truncated) = cap_head_tail(
                                v["newText"].as_str().unwrap_or_default(),
                                3072,
                                1024,
                            );
                            Some(ToolContent::Diff {
                                path: path(&v["path"])?,
                                old_text: v["oldText"]
                                    .as_str()
                                    .map(|s| cap_head_tail(s, 3072, 1024).0),
                                new_text,
                                truncated,
                            })
                        } else if v["content"]["type"] == "text" {
                            let (text, truncated) = cap_head_tail(
                                v["content"]["text"].as_str().unwrap_or_default(),
                                3072,
                                1024,
                            );
                            Some(ToolContent::Output { text, truncated })
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                let content = match content.len() {
                    0 => None,
                    1 => content.into_iter().next(),
                    _ => Some(ToolContent::Batch { diffs: content }),
                };
                step.events.push(AgentEvent::ToolCallUpdate {
                    id,
                    status,
                    content,
                });
            }
            "available_commands_update" => {
                self.commands = unique_rows(&update["availableCommands"], "name")
                    .take(SLASH_COMMANDS_CAP)
                    .map(|c| SlashCommand {
                        name: small(&c["name"]),
                        description: small(&c["description"]),
                        skill_path: None,
                    })
                    .collect();
                step.events.push(self.catalog());
            }
            "config_option_update" => {
                let before_model = self.model.clone();
                let before_mode = self.mode.clone();
                self.set_config(&update["configOptions"]);
                if self.model != before_model {
                    if let Some(value) = self.model.clone() {
                        self.model = before_model;
                        self.changed("model", value, false, &mut step);
                    }
                }
                if self.mode != before_mode {
                    if let Some(value) = self.mode.clone() {
                        self.changed("mode", value, false, &mut step);
                    }
                }
                step.events.extend(self.config_state(false).events);
            }
            "plan" => {
                self.flush_into(&mut step);
                let entries = rows(&update["entries"])
                    .take(64)
                    .map(|e| PlanEntry {
                        content: small(&e["content"]),
                        status: match e["status"].as_str() {
                            Some("completed") => PlanStatus::Done,
                            Some("in_progress") => PlanStatus::InProgress,
                            _ => PlanStatus::Todo,
                        },
                        id: None,
                        active_form: None,
                        description: None,
                        owner: None,
                        blocked_by: Vec::new(),
                    })
                    .collect();
                step.events.push(AgentEvent::Plan { entries });
            }
            "current_mode_update" => {
                self.mode = Some(small(&update["currentModeId"]));
                step.events.push(AgentEvent::ModeChanged {
                    mode_id: self.mode.clone().unwrap(),
                    chosen: false,
                });
            }
            _ => {}
        }
        step
    }
    fn drain_pending(&mut self) -> Vec<AgentEvent> {
        let mut events: Vec<_> = self
            .asks
            .drain()
            .map(|(request_id, _)| AgentEvent::PermissionResolved {
                request_id,
                option_id: "cancelled".into(),
            })
            .collect();
        events.extend(self.queue.drain(..).map(|q| AgentEvent::UserMessageUpdate {
            id: q.id,
            state: UserMessageState::Dropped,
        }));
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn mapper() -> AcpMapper {
        AcpMapper::new("session".into(), &json!({}), &json!({}))
    }
    fn send(mapper: &mut AcpMapper, text: &str) -> DriverStep {
        mapper.on_command(AgentCommand::Send {
            blocks: vec![ContentBlock::Text { text: text.into() }],
        })
    }
    fn permission(id: Value) -> Value {
        json!({"jsonrpc":"2.0","id":id,"method":"session/request_permission","params":{
            "sessionId":"session","toolCall":{"toolCallId":"tool","title":"write file"},
            "options":[{"optionId":"yes","name":"Allow once","kind":"allow_once"},{"optionId":"no","name":"Reject","kind":"reject_once"}]
        }})
    }
    #[test]
    fn fork_is_quiet_and_context_is_sent_once_with_real_prompt() {
        let mut m = mapper();
        m.portable_context = Some("historical copper".into());
        assert!(m.pending.is_empty());
        assert!(m.tick().outbound.is_empty());
        let step = send(&mut m, "continue");
        assert_eq!(
            step.outbound[0]["params"]["prompt"][0]["text"],
            "historical copper"
        );
        assert_eq!(step.outbound[0]["params"]["prompt"][1]["text"], "continue");
        assert!(step
            .events
            .iter()
            .any(|e| matches!(e, AgentEvent::UserMessage {text, ..} if text == "continue")));
        let id = step.outbound[0]["id"].clone();
        m.on_frame(&json!({"id":id,"result":{"stopReason":"end_turn"}}));
        assert_eq!(
            send(&mut m, "again").outbound[0]["params"]["prompt"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn rejected_first_prompt_keeps_context_until_provider_accepts_it() {
        let mut m = mapper();
        m.portable_context = Some("history".into());
        let first = send(&mut m, "continue");
        let rejected = m.on_frame(
            &json!({"id":first.outbound[0]["id"],"error":{"code":-32603,"message":"retry later"}}),
        );
        assert!(!rejected
            .events
            .iter()
            .any(|e| matches!(e, AgentEvent::ForkContextConsumed)));
        let retry = send(&mut m, "retry");
        assert_eq!(retry.outbound[0]["params"]["prompt"][0]["text"], "history");
        let accepted = m.on_frame(&json!({"method":"session/update","params":{"sessionId":"session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"answer"}}}}));
        assert!(accepted
            .events
            .iter()
            .any(|e| matches!(e, AgentEvent::ForkContextConsumed)));
        m.on_frame(&json!({"id":retry.outbound[0]["id"],"error":{"message":"connection lost after answering"}}));
        assert_eq!(
            send(&mut m, "again").outbound[0]["params"]["prompt"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn catalogs_deduplicate_provider_ids_and_tool_content_fits_journal() {
        let mut m = mapper();
        m.set_config(&json!([{"id":"model","category":"model","type":"select","currentValue":"a","options":[{"value":"a","name":"A"},{"value":"a","name":"duplicate"},{"value":"","name":"invalid"}]}]));
        assert_eq!(m.models.len(), 1);
        assert!(
            !m.caps.custom_model,
            "catalog selection does not grant arbitrary model IDs"
        );
        let step = m.on_frame(&json!({"method":"session/update","params":{"sessionId":"session","update":{"sessionUpdate":"available_commands_update","availableCommands":[{"name":"help"},{"name":"help"},{"name":""}]}}}));
        assert_eq!(m.commands.len(), 1);
        assert!(serde_json::to_vec(&step.events[0]).unwrap().len() < 256 * 1024);
        let hostile = "\0".repeat(64 * 1024);
        let content =
            vec![json!({"type":"diff","path":"file","oldText":hostile,"newText":hostile}); 64];
        let step = m.on_frame(&json!({"method":"session/update","params":{"sessionId":"session","update":{"sessionUpdate":"tool_call_update","toolCallId":"tool","content":content}}}));
        for event in step.events {
            assert!(serde_json::to_vec(&event).unwrap().len() < 256 * 1024);
        }
    }
    #[test]
    fn permission_preserves_wire_identity_and_only_offered_choices() {
        let mut m = mapper();
        let step = m.on_frame(&permission(json!(17)));
        let AgentEvent::PermissionRequest { request_id, .. } = &step.events[0] else {
            panic!()
        };
        let id = request_id.clone();
        let cmd = |option: &str| AgentCommand::Permission {
            request_id: id.clone(),
            option_id: option.into(),
            feedback: None,
            destination: None,
        };
        assert!(m.on_command(cmd("invented")).outbound.is_empty());
        let answer = m.on_command(cmd("no"));
        assert_eq!(answer.outbound[0]["id"], 17);
        assert_eq!(answer.outbound[0]["result"]["outcome"]["optionId"], "no");
        assert!(m.on_command(cmd("yes")).outbound.is_empty());
    }
    #[test]
    fn cancel_resolves_asks_and_waits_for_provider_before_next_prompt() {
        let mut m = mapper();
        let first = send(&mut m, "first");
        m.on_frame(&permission(json!("ask")));
        send(&mut m, "queued");
        let step = m.on_command(AgentCommand::Interrupt);
        assert_eq!(
            step.outbound[0]["result"]["outcome"]["outcome"],
            "cancelled"
        );
        assert!(!step
            .outbound
            .iter()
            .any(|v| v["method"] == "session/prompt"));
        let ended =
            m.on_frame(&json!({"id":first.outbound[0]["id"],"result":{"stopReason":"cancelled"}}));
        assert!(ended.events.iter().any(|e| matches!(
            e,
            AgentEvent::TurnAborted {
                interrupted: true,
                ..
            }
        )));
        assert_eq!(ended.outbound[0]["params"]["prompt"][0]["text"], "queued");
    }
    #[test]
    fn send_now_promotes_selected_message_and_stale_ids_do_not_interrupt() {
        let mut m = mapper();
        let first = send(&mut m, "first");
        send(&mut m, "second");
        let third = send(&mut m, "third");
        let AgentEvent::UserMessage { id: Some(id), .. } = &third.events[0] else {
            panic!()
        };
        assert!(m
            .on_command(AgentCommand::SendNow { id: "stale".into() })
            .outbound
            .is_empty());
        m.on_command(AgentCommand::SendNow { id: id.clone() });
        let ended =
            m.on_frame(&json!({"id":first.outbound[0]["id"],"result":{"stopReason":"cancelled"}}));
        assert_eq!(ended.outbound[0]["params"]["prompt"][0]["text"], "third");
    }
    #[test]
    fn config_uses_provider_readback_and_removes_disappeared_controls() {
        let mut m = mapper();
        m.set_config(&json!([{"id":"model","type":"select","category":"model","currentValue":"a","options":[{"value":"a","name":"A"},{"value":"b","name":"B"}]},
            {"id":"effort","type":"select","category":"thought_level","currentValue":"high","options":[{"value":"high","name":"high"}]}]));
        let cmd = m.on_command(AgentCommand::SetModel {
            model_id: "b".into(),
        });
        let step=m.on_frame(&json!({"id":cmd.outbound[0]["id"],"result":{"configOptions":[{"id":"model","type":"select","category":"model","currentValue":"a","options":[{"value":"a","name":"A"}]}]}}));
        assert!(step
            .events
            .iter()
            .any(|e| matches!(e, AgentEvent::ModelSwitched {to,..} if to=="a")));
        assert!(!m.caps.commands.contains(&"set_effort".into()));
        assert!(m.models[0].efforts.is_empty());
    }
    #[test]
    fn teardown_drops_queue_and_resolves_pending_permission() {
        let mut m = mapper();
        send(&mut m, "first");
        send(&mut m, "second");
        m.on_frame(&permission(json!(1)));
        let events = m.drain_pending();
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::UserMessageUpdate {
                state: UserMessageState::Dropped,
                ..
            }
        )));
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::PermissionResolved { .. })));
        assert!(m.drain_pending().is_empty());
    }
    #[test]
    fn capabilities_do_not_invent_controls_or_accept_foreign_sessions() {
        let mut m = mapper();
        assert!(!m.caps.image_input);
        assert!(!m.caps.commands.contains(&"set_model".into()));
        let mut frame = permission(json!(1));
        frame["params"]["sessionId"] = json!("foreign");
        let step = m.on_frame(&frame);
        assert!(step.events.is_empty());
        assert_eq!(
            step.outbound[0]["result"]["outcome"]["outcome"],
            "cancelled"
        );
    }
}
