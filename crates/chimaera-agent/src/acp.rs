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
/// `cursor-agent --version` of the build whose `acp` server was probed
/// (PROTOCOL.md, ACP › Cursor). Turn-level facts await a signed-in run.
pub const TESTED_CURSOR_VERSION: &str = "2026.10.14-941492e";
const MAX_PENDING: usize = 64;
const MAX_CHOICES: usize = 64;

/// A distribution supplies identity/auth policy; the protocol engine is shared.
#[derive(Clone, Copy)]
pub struct AcpAdapter {
    pub kind: &'static str,
    pub tested_version: &'static str,
    pub auth_method: &'static str,
    /// The command a user runs to check this agent by hand.
    pub executable: &'static str,
    /// The product name, for the words a signed-out `authenticate` gets.
    pub name: &'static str,
    /// Whether a failed `authenticate` reads as "not signed in" (Cursor's
    /// method reuses an existing `agent login` and otherwise starts a
    /// browser login whose challenge dies with this process, so its raw
    /// error — "Invalid params" plus a one-time URL — is not actionable).
    /// Only an auth-shaped failure gets the words (`needs_sign_in`); a crash
    /// or transport error keeps its own message.
    pub sign_in: bool,
}
/// Grok's own name for the mode and for the slash command that toggles it.
const ALWAYS_APPROVE: &str = "always-approve";
pub const GROK: AcpAdapter = AcpAdapter {
    kind: "grok",
    tested_version: TESTED_GROK_VERSION,
    auth_method: "cached_token",
    executable: "grok",
    name: "Grok Build",
    sign_in: false,
};
pub const ANTIGRAVITY: AcpAdapter = AcpAdapter {
    kind: "agy",
    tested_version: TESTED_AGY_VERSION,
    auth_method: "oauth-personal",
    executable: "agy",
    name: "Antigravity",
    sign_in: false,
};
pub const CURSOR: AcpAdapter = AcpAdapter {
    kind: "cursor",
    tested_version: TESTED_CURSOR_VERSION,
    auth_method: "cursor_login",
    executable: "cursor-agent",
    name: "Cursor CLI",
    sign_in: true,
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

/// A failed initialization request: the user-facing message, and the
/// JSON-RPC error object when the agent answered with one (`Null` for a
/// transport failure or an exit).
struct RpcError {
    message: String,
    error: Value,
}
impl From<RpcError> for String {
    fn from(e: RpcError) -> String {
        e.message
    }
}
fn transport(message: impl ToString) -> RpcError {
    RpcError {
        message: message.to_string(),
        error: Value::Null,
    }
}

async fn rpc(
    sink: &mut JsonlSink,
    stream: &mut JsonlStream,
    id: u64,
    method: &str,
    params: Value,
) -> Result<Value, RpcError> {
    sink.send(&json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}))
        .await
        .map_err(transport)?;
    loop {
        let frame = stream
            .next()
            .await
            .map_err(transport)?
            .ok_or_else(|| transport("ACP exited during initialization"))?;
        if frame.get("id") == Some(&json!(id)) && frame.get("method").is_none() {
            if let Some(error) = frame.get("error") {
                return Err(RpcError {
                    message: cap_output(error["message"].as_str().unwrap_or("ACP request failed"))
                        .0,
                    error: error.clone(),
                });
            }
            return Ok(frame["result"].clone());
        }
        // Clients advertise no filesystem/terminal callbacks. Refuse unknown
        // requests explicitly so an agent can never hang waiting for us.
        if frame.get("method").is_some() && frame.get("id").is_some() {
            sink.send(&json!({"jsonrpc":"2.0", "id":frame["id"], "error":{"code":-32601,"message":"Client method not supported during initialization"}})).await.map_err(transport)?;
        }
    }
}

/// The two signed-out answers probed from `cursor-agent acp` 2026.10.14
/// (PROTOCOL.md, ACP > Cursor): `-32000 Authentication required`, and the
/// `-32602` whose `data.message` carries the browser-login challenge.
fn needs_sign_in(error: &Value) -> bool {
    let lower = |v: &Value| v.as_str().unwrap_or_default().to_ascii_lowercase();
    match error["code"].as_i64() {
        Some(-32000) => lower(&error["message"]).contains("authentication required"),
        Some(-32602) => lower(&error["data"]["message"]).contains("login"),
        _ => false,
    }
}

impl AcpAdapter {
    /// The words the host and the UI act on (`model::sign_in_failure`): the
    /// chat shows this agent's sign-in card, never a terminal instruction.
    pub fn sign_in_hint(&self) -> String {
        format!(
            "{} is not signed in on this host. Sign in from this chat's card or in Settings → Agents.",
            self.name
        )
    }
    fn authenticate_error(&self, e: RpcError) -> String {
        if self.sign_in && needs_sign_in(&e.error) {
            self.sign_in_hint()
        } else {
            e.message
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
    fn executable(&self) -> &'static str {
        self.executable
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
        .await
        .map_err(|e| self.authenticate_error(e))?;
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
        let mut mapper = AcpMapper::new(native.to_owned(), &init, &session, self.kind);
        mapper.mcp_auto_approve = spec.mcp_auto_approve.clone();
        mapper.offer_grok_modes(self.kind);
        // ACP has no portable system-context field. Hold the bounded handoff
        // until a real user send: opening a fork must stay idle and unbilled.
        mapper.portable_context = spec.portable_context.clone();
        let mut initial = vec![mapper.config_state(false)];
        // Settings use the same acknowledged protocol path as user changes,
        // but restoring one is not a pick: its ack must not overwrite the
        // remembered preference with this conversation's own value.
        mapper.restoring = true;
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
        mapper.restoring = false;
        Ok(Handshake { mapper, initial })
    }
}

struct Ask {
    wire_id: Value,
    options: Vec<PermissionOption>,
}
enum Pending {
    Prompt(String),
    /// The trailing flag is `chosen`: false for a handshake restore.
    Config(String, String, bool),
    Mode(String, bool),
    /// Grok's `/always-approve on|off` prompt: (on, chosen).
    AlwaysApprove(bool, bool),
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
    restoring: bool,
    /// Grok advertises no modes; this adapter offers its always-approve
    /// toggle as one (see `offer_grok_modes`).
    grok_modes: bool,
    always_approve: bool,
    /// An always-approve change travels as a prompt, which Grok runs only
    /// between turns, and its command is listed only after the handshake.
    /// The request waits here (mode id, chosen) until it can be sent.
    deferred_mode: Option<(String, bool)>,
    /// Which agent the frames come from: each names an MCP tool call in its
    /// own pinned shape (`mcp_call`, PROTOCOL.md Pass 45).
    kind: &'static str,
    /// The embedder's standing consent (`SpawnSpec.mcp_auto_approve`),
    /// applied at the permission request — ACP has no pre-allow config.
    mcp_auto_approve: Option<crate::driver::McpAutoApprove>,
    /// Cursor names the MCP server and tool only on the `tool_call_update`
    /// that precedes its permission request (whose `toolCall` carries no
    /// `rawInput`): `toolCallId` → (provider, tool), dropped when the call
    /// ends or the turn does.
    mcp_calls: HashMap<String, (String, String)>,
    /// Calls of the agent's own plan tool: the plan card is their whole
    /// surface, so they draw no row (as claude's TodoWrite).
    hidden_calls: HashSet<String>,
    /// Cursor's to-do list — `cursor/update_todos {merge: true}` upserts by
    /// id into it: (id, content, status) in the order first seen.
    todos: Vec<(String, String, String)>,
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
    fn new(native: String, init: &Value, session: &Value, kind: &'static str) -> Self {
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
            restoring: false,
            grok_modes: false,
            always_approve: false,
            deferred_mode: None,
            kind,
            mcp_auto_approve: None,
            mcp_calls: HashMap::new(),
            hidden_calls: HashSet::new(),
            todos: Vec::new(),
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
    /// Grok 1.0.46 advertises no modes, but its `/always-approve on|off`
    /// command switches approvals mid-session without a model call
    /// (PROTOCOL.md, ACP). While Grok lists that command, offer Normal and
    /// Always-approve, Grok's own names. A session that advertises modes
    /// keeps its own list.
    fn offer_grok_modes(&mut self, kind: &str) {
        self.grok_modes = kind == GROK.kind && self.modes.is_empty();
    }
    /// Called on every command list. The listing itself reads the current
    /// mode back: the handshake's `Init` preceded it with no mode at all.
    fn list_grok_modes(&mut self, step: &mut DriverStep) {
        // A session that starts advertising a mode control owns the list.
        if rows(&self.config).any(|c| c["category"] == "mode") {
            self.grok_modes = false;
            return;
        }
        let listed = self.commands.iter().any(|c| c.name == ALWAYS_APPROVE);
        let before = self.modes.len();
        self.modes = if listed {
            [("default", "Normal"), (ALWAYS_APPROVE, "Always-approve")]
                .into_iter()
                .map(|(id, label)| ModeInfo {
                    id: id.into(),
                    label: label.into(),
                })
                .collect()
        } else {
            Vec::new()
        };
        self.mode = listed.then(|| self.grok_mode().into());
        self.caps.commands.retain(|c| c != "set_mode");
        if listed {
            self.caps.commands.push("set_mode".into());
        }
        if self.modes.len() == before {
            step.events.push(self.catalog());
            return;
        }
        step.events.extend(self.config_state(false).events);
        if listed {
            self.changed("mode", self.grok_mode().into(), false, step);
        }
    }
    fn grok_mode(&self) -> &'static str {
        if self.always_approve {
            ALWAYS_APPROVE
        } else {
            "default"
        }
    }
    /// The switch travels as a `session/prompt`, so it occupies the session
    /// like a turn: sends wait behind it, and it waits behind a turn.
    fn switching(&self) -> bool {
        self.pending
            .values()
            .any(|p| matches!(p, Pending::AlwaysApprove(..)))
    }
    fn busy(&self) -> bool {
        self.turn.is_some() || self.switching()
    }
    fn set_grok_mode(&mut self, target: &str, chosen: bool, step: &mut DriverStep) {
        let offered = self.modes.iter().any(|m| m.id == target);
        if self.turn.is_some() {
            step.events.push(AgentEvent::Notice {
                text: "Grok changes this between turns. It will switch when this turn ends.".into(),
            });
        }
        // A restore runs before Grok has listed its commands.
        if self.busy() || (!offered && self.restoring) {
            self.deferred_mode = Some((target.into(), chosen));
            return;
        }
        if !offered {
            step.events.push(AgentEvent::Notice {
                text: "This control is not offered by the agent".into(),
            });
            return;
        }
        let on = target == ALWAYS_APPROVE;
        if on == self.always_approve {
            // Already there: a pick still reads back, a restore has nothing to say.
            if chosen {
                self.changed("mode", target.into(), true, step);
            }
            return;
        }
        let text = format!("/{ALWAYS_APPROVE} {}", if on { "on" } else { "off" });
        self.request(
            "session/prompt",
            json!({"sessionId":self.native,"prompt":[{"type":"text","text":text}]}),
            Pending::AlwaysApprove(on, chosen),
            step,
        );
    }
    /// The session is free: first a held mode switch, then the next queued
    /// send — never both in one step, as each is a `session/prompt`.
    fn resume(&mut self, step: &mut DriverStep) {
        if self.busy() {
            return;
        }
        let listed = self.modes.iter().any(|m| m.id == ALWAYS_APPROVE);
        if let Some((target, chosen)) = self.deferred_mode.take_if(|_| listed) {
            self.set_grok_mode(&target, chosen, step);
        }
        if !self.busy() {
            if let Some(item) = self.queue.pop_front() {
                self.start(item, true, step);
            }
        }
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
                sign_in: false,
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
                sign_in: false,
            });
            return;
        }
        let mut prompt: Vec<Value> = item
            .blocks
            .iter()
            // A blank block reads to the agent as a message that came through
            // empty (ingress strips them; this is the backstop).
            .filter(|b| b.has_content())
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
    /// The MCP server a permission request names, and the tool when the
    /// whole frame has this agent's pinned shape (live-captured, PROTOCOL.md
    /// Pass 45) — `None` for anything that is not an MCP call (a shell
    /// command, a file edit, an agent this adapter has no shape for). The
    /// model-chosen tool name is interpolated verbatim into every title, so
    /// the title is checked against the structured fields, never parsed on
    /// its own; the tool keeps a quote-free charset.
    fn mcp_call(&self, call: &Value) -> Option<(String, Option<String>)> {
        let title = call["title"].as_str()?;
        let plain = |s: &str| {
            !s.is_empty()
                && s.len() <= 128
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        };
        let (server, tool, pinned) = match self.kind {
            // agy-acp 1.2.1: `_meta {mcp: {server, tool}, is_mcp_tool_call}`,
            // title `<server>_<tool>`.
            k if k == ANTIGRAVITY.kind => {
                let meta = &call["_meta"];
                if meta["is_mcp_tool_call"] != true {
                    return None;
                }
                let server = identifier(&meta["mcp"]["server"])?;
                let tool = identifier(&meta["mcp"]["tool"])?;
                let pinned = title == format!("{server}_{tool}");
                (server, tool, pinned)
            }
            // grok 1.0.46: `rawInput {variant: "UseTool", tool_name:
            // "<server>__<tool>"}` under `_meta["x.ai/tool"].kind ==
            // "use_tool"`; the title is the tool_name again.
            k if k == GROK.kind => {
                let input = &call["rawInput"];
                if input["variant"] != "UseTool" || call["_meta"]["x.ai/tool"]["kind"] != "use_tool"
                {
                    return None;
                }
                let name = identifier(&input["tool_name"])?;
                let (server, tool) = name.split_once("__")?;
                (server.to_owned(), tool.to_owned(), title == name)
            }
            // cursor 2026.10.14: the request's toolCall has no rawInput; the
            // `tool_call_update` before it said `{providerIdentifier,
            // toolName}` (kept in `mcp_calls`), and the title must agree:
            // `<server>-<tool>: <tool>`.
            k if k == CURSOR.kind => {
                let (provider, tool) = self.mcp_calls.get(call["toolCallId"].as_str()?)?;
                let pinned = title == format!("{provider}-{tool}: {tool}");
                (provider.clone(), tool.clone(), pinned)
            }
            _ => return None,
        };
        if !plain(&server) {
            return None;
        }
        Some((server, (pinned && plain(&tool)).then_some(tool)))
    }
    /// The embedder's standing consent, answered at the prompt: the id of
    /// an offered `allow_once` option — never `allow_always`, which the agent
    /// would keep past this process — or `None` to surface the ask. Scoped
    /// hard, as the codex driver's: one server; a listed tool only in the
    /// exact pinned shape (a reworded title, with a consent, warns so drift
    /// is diagnosable); whole-server consent needs the structured server
    /// match alone.
    fn pre_approved(&self, call: &Value, options: &[PermissionOption]) -> Option<String> {
        let allow = self.mcp_auto_approve.as_ref()?;
        let (server, tool) = self.mcp_call(call)?;
        if server != allow.server {
            return None;
        }
        let approved = match (&allow.tools, tool) {
            (None, _) => true,
            (Some(list), Some(tool)) => list.contains(&tool),
            (Some(_), None) => {
                tracing::warn!(
                    agent = self.kind,
                    title = call["title"].as_str().unwrap_or_default(),
                    "ACP MCP permission request did not match the pinned shape; \
                     surfacing the prompt instead of pre-approving"
                );
                false
            }
        };
        if !approved {
            return None;
        }
        options
            .iter()
            .find(|o| o.kind == PermissionOptionKind::AllowOnce)
            .map(|o| o.id.clone())
    }
    /// Fold a `cursor/update_todos` into the session's list (`merge: false`
    /// replaces it, `true` upserts by id, order of first sight) and return
    /// the plan card's entries. The card has no "cancelled" state, so a
    /// cancelled to-do leaves it; the list keeps it for the reply.
    fn cursor_todos(&mut self, params: &Value) -> Vec<PlanEntry> {
        if params["merge"] != true {
            self.todos.clear();
        }
        for todo in rows(&params["todos"]) {
            let Some(id) = identifier(&todo["id"]) else {
                continue;
            };
            let content = cap_head_tail(todo["content"].as_str().unwrap_or_default(), 2048, 0).0;
            let status = small(&todo["status"]);
            if let Some(row) = self.todos.iter_mut().find(|r| r.0 == id) {
                row.1 = content;
                row.2 = status;
            } else if self.todos.len() < PLAN_TASKS_CAP {
                self.todos.push((id, content, status));
            }
        }
        self.todos
            .iter()
            .filter(|(_, _, status)| status != "cancelled")
            .map(|(id, content, status)| PlanEntry {
                content: truncate_label(content, PLAN_LABEL_MAX),
                status: crate::claude::plan_status(status),
                id: Some(id.clone()),
                ..Default::default()
            })
            .collect()
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
                Pending::Config(category.into(), value.into(), !self.restoring),
                step,
            );
        } else if category == "mode" && self.grok_modes {
            self.set_grok_mode(value, !self.restoring, step);
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
                    Pending::Mode(value.into(), !self.restoring),
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
                        sign_in: false,
                    });
                    return step;
                }
                if self.busy() {
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
                    self.mcp_calls.clear();
                    self.hidden_calls.clear();
                    for (request_id, _) in self.asks.drain() {
                        step.events.push(AgentEvent::PermissionResolved {
                            request_id,
                            option_id: "cancelled".into(),
                        });
                    }
                    self.resume(&mut step);
                }
                // The command ran inside Grok when no model call was billed
                // for it; anything else was read as an ordinary message.
                Pending::AlwaysApprove(on, chosen) => {
                    let result = &frame["result"];
                    if let Some(message) = error {
                        step.events.push(AgentEvent::Error {
                            message,
                            fatal: false,
                            sign_in: false,
                        });
                    } else if result["_meta"]["totalTokens"] == 0
                        && result["stopReason"] != "cancelled"
                    {
                        self.always_approve = on;
                        self.changed("mode", self.grok_mode().into(), chosen, &mut step);
                    } else {
                        step.events.push(AgentEvent::Notice {
                            text: "Grok did not change always-approve".into(),
                        });
                    }
                    self.resume(&mut step);
                }
                _ if error.is_some() => step.events.push(AgentEvent::Error {
                    message: error.unwrap(),
                    fatal: false,
                    sign_in: false,
                }),
                Pending::Config(category, value, chosen) => {
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
                    self.changed(&category, value, chosen, &mut step);
                    step.events.extend(self.config_state(false).events);
                }
                Pending::Model(value) => self.changed("model", value, true, &mut step),
                Pending::Mode(value, chosen) => self.changed("mode", value, chosen, &mut step),
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
            let call = &params["toolCall"];
            if params["sessionId"] == self.native {
                if let Some(option) = self.pre_approved(call, &options) {
                    step.outbound.push(json!({"jsonrpc":"2.0","id":frame["id"],"result":{"outcome":{"outcome":"selected","optionId":option}}}));
                    return step;
                }
            }
            if params["sessionId"] != self.native
                || self.asks.len() >= MAX_PENDING
                || options.is_empty()
            {
                step.outbound.push(json!({"jsonrpc":"2.0","id":frame["id"],"result":{"outcome":{"outcome":"cancelled"}}}));
            } else {
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
        // Cursor's to-do list IS its plan (PROTOCOL.md Pass 45): the agent's
        // `TodoWrite` reaches the client as `cursor/update_todos` — a request
        // on 2026.10.14 (the docs call it a notification; either is answered
        // the same) — and draws the plan card like every other level-set.
        if frame["method"] == "cursor/update_todos" {
            self.flush_into(&mut step);
            let entries = self.cursor_todos(params);
            step.events.push(AgentEvent::Plan { entries });
            if frame.get("id").is_some() {
                let todos: Vec<Value> = self
                    .todos
                    .iter()
                    .map(|(id, content, status)| json!({"id":id,"content":content,"status":status}))
                    .collect();
                step.outbound.push(json!({"jsonrpc":"2.0","id":frame["id"],"result":{"outcome":{"outcome":"accepted","todos":todos}}}));
            }
            return step;
        }
        if frame.get("id").is_some() {
            step.outbound.push(json!({"jsonrpc":"2.0","id":frame["id"],"error":{"code":-32601,"message":"Client method not supported"}}));
            // Cursor's blocking extensions (PROTOCOL.md, ACP › Cursor) stop
            // its turn until answered; say why the agent moved on.
            let asked = match frame["method"].as_str() {
                Some("cursor/ask_question") => Some("a question"),
                Some("cursor/create_plan") => Some("plan approval"),
                _ => None,
            };
            if let Some(asked) = asked {
                self.flush_into(&mut step);
                step.events.push(AgentEvent::Notice {
                    text: format!(
                        "Cursor asked for {asked}, which chat cannot show yet, so the request was declined. Use the terminal view for turns that need questions or plan approval."
                    ),
                });
            }
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
                let ended = matches!(status, ToolStatus::Completed | ToolStatus::Failed);
                // Cursor's `tool_call_update` is where an MCP call names its
                // server and tool (`rawInput {providerIdentifier, toolName}`);
                // the permission request that follows carries only a title.
                if self.kind == CURSOR.kind {
                    let input = &update["rawInput"];
                    if let (Some(provider), Some(tool)) = (
                        identifier(&input["providerIdentifier"]),
                        identifier(&input["toolName"]),
                    ) {
                        if self.mcp_calls.len() < MAX_PENDING || self.mcp_calls.contains_key(&id) {
                            self.mcp_calls.insert(id.clone(), (provider, tool));
                        }
                    }
                    if ended {
                        self.mcp_calls.remove(&id);
                    }
                }
                // The agent's own plan tool — Cursor's `updateTodos` (its
                // list arrives as `cursor/update_todos`), Grok's `todo_write`
                // (its list arrives as the `plan` update) — is the plan
                // card's bookkeeping and draws no row, like claude's TodoWrite.
                if update["sessionUpdate"] == "tool_call"
                    && (update["rawInput"]["_toolName"] == "updateTodos"
                        || update["_meta"]["x.ai/tool"]["kind"] == "plan")
                {
                    if self.hidden_calls.len() < MAX_PENDING {
                        self.hidden_calls.insert(id);
                    }
                    return step;
                }
                if self.hidden_calls.contains(&id) {
                    if ended {
                        self.hidden_calls.remove(&id);
                    }
                    return step;
                }
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
                if self.grok_modes {
                    self.list_grok_modes(&mut step);
                    self.resume(&mut step);
                }
                if !self.grok_modes {
                    step.events.push(self.catalog());
                }
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
        AcpMapper::new("session".into(), &json!({}), &json!({}), "")
    }
    fn commands(m: &mut AcpMapper, names: &[&str]) -> DriverStep {
        let list: Vec<_> = names.iter().map(|n| json!({"name":n})).collect();
        m.on_frame(&json!({"method":"session/update","params":{"sessionId":"session","update":{"sessionUpdate":"available_commands_update","availableCommands":list}}}))
    }
    fn mode_events(step: &DriverStep) -> Vec<(String, bool)> {
        step.events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ModeChanged { mode_id, chosen } => Some((mode_id.clone(), *chosen)),
                _ => None,
            })
            .collect()
    }
    #[test]
    fn grok_offers_always_approve_while_its_command_is_listed() {
        let mut m = mapper();
        m.offer_grok_modes(GROK.kind);
        assert!(m.modes.is_empty());
        let step = commands(&mut m, &["compact", "always-approve"]);
        assert_eq!(
            m.modes.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["default", "always-approve"]
        );
        assert_eq!(m.mode.as_deref(), Some("default"));
        assert!(step.events.iter().any(|e| matches!(e, AgentEvent::Capabilities { capabilities } if capabilities.commands.contains(&"set_mode".into()))));
        // Init carried no mode: the listing reads the current one back.
        assert_eq!(mode_events(&step), [("default".to_string(), false)]);
        // Picking the mode already current sends nothing and still reads back.
        let step = m.on_command(AgentCommand::SetMode {
            mode_id: "default".into(),
        });
        assert!(step.outbound.is_empty());
        assert_eq!(mode_events(&step), [("default".to_string(), true)]);

        let step = m.on_command(AgentCommand::SetMode {
            mode_id: "always-approve".into(),
        });
        assert_eq!(step.outbound[0]["method"], "session/prompt");
        assert_eq!(
            step.outbound[0]["params"]["prompt"][0]["text"],
            "/always-approve on"
        );
        // No turn, no user message: the switch is not conversation.
        assert!(step.events.is_empty());
        // The switch is a prompt: a send meanwhile waits behind it.
        let queued = send(&mut m, "hi");
        assert!(queued.outbound.is_empty());
        assert!(queued
            .events
            .iter()
            .any(|e| matches!(e, AgentEvent::UserMessage { queued: true, .. })));
        let acked = m.on_frame(&json!({"id":step.outbound[0]["id"],"result":{"stopReason":"end_turn","_meta":{"totalTokens":0}}}));
        assert_eq!(mode_events(&acked), [("always-approve".to_string(), true)]);
        assert_eq!(acked.outbound[0]["params"]["prompt"][0]["text"], "hi");
        assert!(m.turn.is_some());
        m.on_frame(&json!({"id":acked.outbound[0]["id"],"result":{"stopReason":"end_turn"}}));

        // Tokens were billed: Grok read the text as a message, not a command.
        let step = m.on_command(AgentCommand::SetMode {
            mode_id: "default".into(),
        });
        assert_eq!(
            step.outbound[0]["params"]["prompt"][0]["text"],
            "/always-approve off"
        );
        let acked = m.on_frame(&json!({"id":step.outbound[0]["id"],"result":{"stopReason":"end_turn","_meta":{"totalTokens":120}}}));
        assert!(mode_events(&acked).is_empty());
        assert!(m.always_approve);

        // A cancelled prompt ran nothing; an error frees the session too.
        let step = m.on_command(AgentCommand::SetMode {
            mode_id: "default".into(),
        });
        let acked = m.on_frame(&json!({"id":step.outbound[0]["id"],"result":{"stopReason":"cancelled","_meta":{"totalTokens":0}}}));
        assert!(mode_events(&acked).is_empty());
        assert!(m.always_approve);
        let step = m.on_command(AgentCommand::SetMode {
            mode_id: "default".into(),
        });
        send(&mut m, "after");
        let acked = m.on_frame(&json!({"id":step.outbound[0]["id"],"error":{"message":"nope"}}));
        assert!(acked
            .events
            .iter()
            .any(|e| matches!(e, AgentEvent::Error { .. })));
        assert_eq!(acked.outbound[0]["params"]["prompt"][0]["text"], "after");

        // The command disappears: so does the control.
        commands(&mut m, &["compact"]);
        assert!(m.modes.is_empty());
        assert!(!m.caps.commands.contains(&"set_mode".into()));
    }
    #[test]
    fn grok_always_approve_waits_for_the_turn_and_for_the_command_list() {
        let mut m = mapper();
        m.offer_grok_modes(GROK.kind);
        // A restore at the handshake precedes the command list, and is not a pick.
        m.restoring = true;
        let step = m.on_command(AgentCommand::SetMode {
            mode_id: "always-approve".into(),
        });
        m.restoring = false;
        assert!(step.outbound.is_empty());
        let step = commands(&mut m, &["always-approve"]);
        assert_eq!(
            step.outbound[0]["params"]["prompt"][0]["text"],
            "/always-approve on"
        );
        let acked =
            m.on_frame(&json!({"id":step.outbound[0]["id"],"result":{"_meta":{"totalTokens":0}}}));
        assert_eq!(mode_events(&acked), [("always-approve".to_string(), false)]);

        // Mid-turn, the switch is held until the turn's prompt returns.
        let turn = send(&mut m, "hi");
        let step = m.on_command(AgentCommand::SetMode {
            mode_id: "default".into(),
        });
        assert!(step.outbound.is_empty());
        assert!(step
            .events
            .iter()
            .any(|e| matches!(e, AgentEvent::Notice { .. })));
        // A message queued behind the turn goes out after the switch, not with it.
        send(&mut m, "next");
        let done =
            m.on_frame(&json!({"id":turn.outbound[0]["id"],"result":{"stopReason":"end_turn"}}));
        assert_eq!(done.outbound.len(), 1);
        assert_eq!(
            done.outbound[0]["params"]["prompt"][0]["text"],
            "/always-approve off"
        );
        let acked = m.on_frame(&json!({"id":done.outbound[0]["id"],"result":{"stopReason":"end_turn","_meta":{"totalTokens":0}}}));
        assert_eq!(mode_events(&acked), [("default".to_string(), true)]);
        assert_eq!(acked.outbound[0]["params"]["prompt"][0]["text"], "next");

        // A restore of the mode already current is silent: no prompt, no pick.
        let mut fresh = mapper();
        fresh.offer_grok_modes(GROK.kind);
        fresh.restoring = true;
        fresh.on_command(AgentCommand::SetMode {
            mode_id: "default".into(),
        });
        fresh.restoring = false;
        let step = commands(&mut fresh, &["always-approve"]);
        assert!(step.outbound.is_empty());
        assert_eq!(mode_events(&step), [("default".to_string(), false)]);

        // Other adapters, and a session with its own modes, are untouched.
        let mut agy = mapper();
        agy.offer_grok_modes(ANTIGRAVITY.kind);
        commands(&mut agy, &["always-approve"]);
        assert!(agy.modes.is_empty());
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

    /// Shapes probed from `cursor-agent acp` 2026.10.14-941492e while signed
    /// out (PROTOCOL.md, ACP › Cursor); its documented modes are agent,
    /// plan and ask. Nothing beyond what the session advertises is offered.
    #[test]
    fn cursor_offers_only_what_its_session_advertises() {
        let init = json!({"protocolVersion":1,"agentCapabilities":{"loadSession":true,"promptCapabilities":{"image":true,"audio":false,"embeddedContext":false},"sessionCapabilities":{"list":{}}},"authMethods":[{"id":"cursor_login"}]});
        let session = json!({"sessionId":"session","modes":{"currentModeId":"agent","availableModes":[{"id":"agent","name":"Agent"},{"id":"plan","name":"Plan"},{"id":"ask","name":"Ask"}]}});
        let mut m = AcpMapper::new("session".into(), &init, &session, CURSOR.kind);
        m.offer_grok_modes(CURSOR.kind);
        assert!(!m.grok_modes);
        assert!(m.caps.image_input);
        assert!(m.caps.commands.contains(&"set_mode".into()));
        assert!(!m.caps.commands.contains(&"set_model".into()));
        assert!(!m.caps.custom_model);
        let ids: Vec<_> = m.modes.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["agent", "plan", "ask"]);
        let step = m.on_command(AgentCommand::SetMode {
            mode_id: "plan".into(),
        });
        assert_eq!(step.outbound[0]["method"], "session/set_mode");
        let step = m.on_command(AgentCommand::SetMode {
            mode_id: "yolo".into(),
        });
        assert!(step.outbound.is_empty());
    }

    #[test]
    fn cursor_blocking_extensions_are_refused_with_a_visible_reason() {
        let mut m = mapper();
        let step = m.on_frame(&json!({"jsonrpc":"2.0","id":7,"method":"cursor/ask_question","params":{"toolCallId":"t","title":"Pick","questions":[]}}));
        assert_eq!(step.outbound[0]["error"]["code"], -32601);
        assert!(step
            .events
            .iter()
            .any(|e| matches!(e, AgentEvent::Notice { text }
                if text.contains("was declined") && text.contains("terminal view"))));
        let step =
            m.on_frame(&json!({"jsonrpc":"2.0","id":8,"method":"fs/read_text_file","params":{}}));
        assert_eq!(step.outbound[0]["error"]["code"], -32601);
        assert!(step.events.is_empty());
    }

    #[test]
    fn only_a_signed_out_answer_becomes_the_sign_in_hint() {
        let failure = |error: Value| RpcError {
            message: cap_output(error["message"].as_str().unwrap_or("ACP request failed")).0,
            error,
        };
        let hint = CURSOR.sign_in_hint();
        assert!(
            crate::model::sign_in_failure(&hint),
            "the hint is itself a cue"
        );
        // The probed signed-out answers.
        for error in [
            json!({"code":-32000,"message":"Authentication required","data":{"message":"Authentication required. Please run 'agent login' first"}}),
            json!({"code":-32602,"message":"Invalid params","data":{"message":"Failed to open browser for login. Please visit: https://cursor.com/loginDeepControl?challenge=x"}}),
        ] {
            assert_eq!(CURSOR.authenticate_error(failure(error.clone())), hint);
            // Other agents never borrow Cursor's advice.
            assert_ne!(GROK.authenticate_error(failure(error)), hint);
        }
        // A crash, a transport failure or an unrelated error keeps its own words.
        assert_eq!(
            CURSOR.authenticate_error(transport("ACP exited during initialization")),
            "ACP exited during initialization"
        );
        assert_eq!(
            CURSOR.authenticate_error(failure(
                json!({"code":-32603,"message":"Internal error: ECONNRESET"})
            )),
            "Internal error: ECONNRESET"
        );
        assert_eq!(
            CURSOR.authenticate_error(failure(
                json!({"code":-32602,"message":"Invalid params","data":{"message":"methodId must be a string"}})
            )),
            "Invalid params"
        );
        assert_eq!(Driver::executable(&CURSOR), "cursor-agent");
        assert_eq!(Driver::executable(&GROK), "grok");
    }

    fn consent(tools: Option<&[&str]>) -> crate::driver::McpAutoApprove {
        crate::driver::McpAutoApprove {
            server: "chimaera".into(),
            tools: tools.map(|t| t.iter().map(|s| s.to_string()).collect()),
        }
    }
    fn update(update: Value) -> Value {
        json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session","update":update}})
    }
    fn ask(id: u64, tool_call: Value, options: Value) -> Value {
        json!({"jsonrpc":"2.0","id":id,"method":"session/request_permission","params":{"sessionId":"session","toolCall":tool_call,"options":options}})
    }
    fn selected(step: &DriverStep, id: u64, option: &str) -> bool {
        step.events.is_empty()
            && step.outbound.len() == 1
            && step.outbound[0]["id"] == id
            && step.outbound[0]["result"]["outcome"]
                == json!({"outcome":"selected","optionId":option})
    }
    fn surfaced(step: &DriverStep) -> bool {
        step.outbound.is_empty()
            && matches!(
                step.events.as_slice(),
                [AgentEvent::PermissionRequest { .. }]
            )
    }

    /// The embedder's consent, answered in each agent's live-captured shape
    /// (PROTOCOL.md Pass 45): a listed chimaera tool runs with no
    /// PermissionRequest, answered with the offered allow_once (never
    /// allow_always); an unlisted tool, another server, a reworded title
    /// and an ordinary tool call still surface; without a consent nothing
    /// is answered.
    #[test]
    fn antigravity_pre_approval_reads_the_mcp_meta_and_the_title() {
        let mut m = mapper();
        m.kind = ANTIGRAVITY.kind;
        m.mcp_auto_approve = Some(consent(Some(&["notify", "open_browser"])));
        // agy-acp 1.2.1, captured 2026-10-15.
        let options = json!([{"optionId":"allow_always","name":"Allow Always","kind":"allow_always"},{"optionId":"allow","name":"Allow","kind":"allow_once"},{"optionId":"deny","name":"Deny","kind":"reject_once"}]);
        let call = |title: &str, server: &str, tool: &str| {
            json!({"toolCallId":"cab0087fdc5748f3b428d0cc480cd5a7","kind":"other","status":"pending","title":title,"content":[],
                "rawInput":{"arguments":{"text":"hello from probe"},"text":"hello from probe"},
                "_meta":{"mcp":{"tool":tool,"server":server},"is_mcp_tool_call":true}})
        };
        let step = m.on_frame(&ask(
            0,
            call("chimaera_notify", "chimaera", "notify"),
            options.clone(),
        ));
        assert!(selected(&step, 0, "allow"), "{:?}", step.events);
        assert!(m.asks.is_empty());
        assert!(surfaced(&m.on_frame(&ask(
            1,
            call("chimaera_list_terminals", "chimaera", "list_terminals"),
            options.clone()
        ))));
        // Another server's tool of a listed name.
        assert!(surfaced(&m.on_frame(&ask(
            2,
            call("other_notify", "other", "notify"),
            options.clone()
        ))));
        // The structured fields say notify but the title doesn't: no match.
        assert!(surfaced(&m.on_frame(&ask(
            3,
            call("chimaera_list_terminals", "chimaera", "notify"),
            options.clone()
        ))));
        // A shell command is never an MCP call.
        assert!(surfaced(&m.on_frame(&ask(
            4,
            json!({"toolCallId":"t","kind":"execute","title":"ls","rawInput":{"command":"ls"}}),
            options.clone()
        ))));
        // Whole-server consent: the structured server match alone.
        m.mcp_auto_approve = Some(consent(None));
        let step = m.on_frame(&ask(
            5,
            call("chimaera_list_terminals", "chimaera", "list_terminals"),
            options.clone(),
        ));
        assert!(selected(&step, 5, "allow"));
        assert!(surfaced(&m.on_frame(&ask(
            6,
            call("other_notify", "other", "notify"),
            options.clone()
        ))));
        // No allow_once offered: surface rather than pick a standing grant.
        let only_always = json!([{"optionId":"allow_always","name":"Allow Always","kind":"allow_always"},{"optionId":"deny","name":"Deny","kind":"reject_once"}]);
        assert!(surfaced(&m.on_frame(&ask(
            7,
            call("chimaera_notify", "chimaera", "notify"),
            only_always
        ))));
        // No consent at all.
        m.mcp_auto_approve = None;
        assert!(surfaced(&m.on_frame(&ask(
            8,
            call("chimaera_notify", "chimaera", "notify"),
            options
        ))));
    }

    #[test]
    fn grok_pre_approval_reads_the_use_tool_input_and_the_title() {
        let mut m = mapper();
        m.kind = GROK.kind;
        m.mcp_auto_approve = Some(consent(Some(&["notify"])));
        // grok 1.0.46, captured 2026-10-15.
        let options = json!([{"optionId":"always-allow","name":"always allow","kind":"allow_always"},{"optionId":"allow-once","name":"allow once","kind":"allow_once"},{"optionId":"reject-once","name":"reject once","kind":"reject_once"}]);
        let call = |title: &str, name: &str| {
            json!({"toolCallId":"call-a49db86c-3bb4-445f-887b-27c3370ef139-1","kind":"other","title":title,
                "rawInput":{"variant":"UseTool","tool_name":name,"tool_input":{"text":"hello from probe"}},
                "_meta":{"x.ai/tool":{"version":1,"name":"use_tool","kind":"use_tool","namespace":"grok_build","label":"Use Tool","read_only":false}}})
        };
        let step = m.on_frame(&ask(
            0,
            call("chimaera__notify", "chimaera__notify"),
            options.clone(),
        ));
        assert!(selected(&step, 0, "allow-once"), "{:?}", step.events);
        assert!(surfaced(&m.on_frame(&ask(
            1,
            call("chimaera__list_terminals", "chimaera__list_terminals"),
            options.clone()
        ))));
        assert!(surfaced(&m.on_frame(&ask(
            2,
            call("other__notify", "other__notify"),
            options.clone()
        ))));
        // A title that disagrees with the structured name.
        assert!(surfaced(&m.on_frame(&ask(
            3,
            call("chimaera__notify", "chimaera__list_terminals"),
            options.clone()
        ))));
        // Grok's own plan tool is not a use_tool frame.
        assert!(surfaced(&m.on_frame(&ask(
            4,
            json!({"toolCallId":"t","title":"chimaera__notify","rawInput":{"variant":"UseTool","tool_name":"chimaera__notify"},"_meta":{"x.ai/tool":{"kind":"plan"}}}),
            options.clone()
        ))));
        // Whole-server consent still needs the structured server.
        m.mcp_auto_approve = Some(consent(None));
        assert!(selected(
            &m.on_frame(&ask(
                5,
                call("chimaera__list_terminals", "chimaera__list_terminals"),
                options.clone()
            )),
            5,
            "allow-once"
        ));
        assert!(surfaced(&m.on_frame(&ask(
            6,
            call("other__notify", "other__notify"),
            options
        ))));
    }

    #[test]
    fn cursor_pre_approval_needs_the_preceding_update_and_the_title_to_agree() {
        let mut m = mapper();
        m.kind = CURSOR.kind;
        m.mcp_auto_approve = Some(consent(Some(&["notify"])));
        // cursor-agent 2026.10.14-941492e, captured 2026-10-15: the id
        // carries a newline; the request's toolCall has no rawInput.
        let id = "call-dff99f2e-b536-4ea5-82b2-409a16e88c4c-1\nfc_77bc5181-0275-9386-833a-574b551b67aa_0";
        let options = json!([{"optionId":"allow-once","name":"Allow once","kind":"allow_once"},{"optionId":"allow-always","name":"Allow always","kind":"allow_always"},{"optionId":"reject-once","name":"Reject","kind":"reject_once"}]);
        let named = |id: &str, provider: &str, tool: &str| {
            update(
                json!({"sessionUpdate":"tool_call_update","toolCallId":id,"title":format!("{provider}: {tool}"),
                "rawInput":{"providerIdentifier":provider,"toolName":tool,"args":{"text":"hello from probe"}}}),
            )
        };
        let request = |n: u64, id: &str, title: &str| {
            ask(
                n,
                json!({"toolCallId":id,"title":title,"kind":"other","status":"pending",
                "content":[{"type":"content","content":{"type":"text","text":"```json\n{\n  \"text\": \"hello from probe\"\n}\n```"}}]}),
                options.clone(),
            )
        };
        // The row opens as "MCP: tool", then the update names the call.
        m.on_frame(&update(json!({"sessionUpdate":"tool_call","toolCallId":id,"title":"MCP: tool","kind":"other","status":"pending","rawInput":{}})));
        m.on_frame(&named(id, "chimaera", "notify"));
        let step = m.on_frame(&request(0, id, "chimaera-notify: notify"));
        assert!(selected(&step, 0, "allow-once"), "{:?}", step.events);
        // The same id, completed: a later request for it surfaces.
        m.on_frame(&update(json!({"sessionUpdate":"tool_call_update","toolCallId":id,"status":"completed","rawOutput":{"success":true}})));
        assert!(surfaced(&m.on_frame(&request(
            1,
            id,
            "chimaera-notify: notify"
        ))));
        // A title alone never matches: no update named the call.
        assert!(surfaced(&m.on_frame(&request(
            2,
            "call-x",
            "chimaera-notify: notify"
        ))));
        // The update and the title disagree.
        m.on_frame(&named("call-y", "chimaera", "list_terminals"));
        assert!(surfaced(&m.on_frame(&request(
            3,
            "call-y",
            "chimaera-notify: notify"
        ))));
        // Unlisted, and another provider's tool of the listed name.
        m.on_frame(&named("call-z", "chimaera", "list_terminals"));
        assert!(surfaced(&m.on_frame(&request(
            4,
            "call-z",
            "chimaera-list_terminals: list_terminals"
        ))));
        m.on_frame(&named("call-w", "other", "notify"));
        assert!(surfaced(&m.on_frame(&request(
            5,
            "call-w",
            "other-notify: notify"
        ))));
        // A turn's end forgets what it named.
        m.on_frame(&named("call-v", "chimaera", "notify"));
        let prompt = send(&mut m, "go");
        m.on_frame(&json!({"id":prompt.outbound[0]["id"],"result":{"stopReason":"end_turn"}}));
        assert!(m.mcp_calls.is_empty());
        assert!(surfaced(&m.on_frame(&request(
            6,
            "call-v",
            "chimaera-notify: notify"
        ))));
    }

    /// Cursor's to-do list is the plan card (PROTOCOL.md Pass 45): the
    /// captured `cursor/update_todos` request draws a Plan level-set and is
    /// answered with the documented accepted outcome; `merge: true` upserts
    /// by id; a cancelled item leaves the card; the agent's own
    /// `updateTodos` rows draw nothing.
    #[test]
    fn cursor_update_todos_draws_the_plan_card_and_is_accepted() {
        let mut m = mapper();
        m.kind = CURSOR.kind;
        let id = "call-e7813dbd-f14c-4d36-9bc1-581803d671d3-1\nfc_20676f0e-24ad-9ed8-8c29-cde7d2c8d719_0";
        // The tool rows around the list: hidden, their updates too.
        let opened = m.on_frame(&update(json!({"sessionUpdate":"tool_call","toolCallId":id,"title":"Update TODOs","kind":"other","status":"pending","rawInput":{"_toolName":"updateTodos"}})));
        assert!(opened.events.is_empty(), "{:?}", opened.events);
        let updated = m.on_frame(&update(json!({"sessionUpdate":"tool_call_update","toolCallId":id,"title":"Update TODOs: inspect, draft, verify",
            "rawInput":{"_toolName":"updateTodos","todos":[{"id":"inspect","content":"inspect","status":"TODO_STATUS_IN_PROGRESS","createdAt":"1792051486990","updatedAt":"1792051488874","dependencies":[]}]}})));
        assert!(updated.events.is_empty(), "{:?}", updated.events);
        let done = m.on_frame(&update(
            json!({"sessionUpdate":"tool_call_update","toolCallId":id,"status":"completed"}),
        ));
        assert!(done.events.is_empty() && m.hidden_calls.is_empty());
        // The captured request, verbatim.
        let step = m.on_frame(&json!({"jsonrpc":"2.0","id":0,"method":"cursor/update_todos","params":{"toolCallId":id,
            "todos":[{"id":"inspect","content":"inspect","status":"pending"},{"id":"draft","content":"draft","status":"pending"},{"id":"verify","content":"verify","status":"pending"}],"merge":false}}));
        let [AgentEvent::Plan { entries }] = step.events.as_slice() else {
            panic!("{:?}", step.events);
        };
        assert_eq!(
            entries
                .iter()
                .map(|e| (e.id.as_deref().unwrap(), e.status))
                .collect::<Vec<_>>(),
            [
                ("inspect", PlanStatus::Todo),
                ("draft", PlanStatus::Todo),
                ("verify", PlanStatus::Todo)
            ]
        );
        assert_eq!(step.outbound[0]["id"], 0);
        assert_eq!(step.outbound[0]["result"]["outcome"]["outcome"], "accepted");
        assert_eq!(
            step.outbound[0]["result"]["outcome"]["todos"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        // merge: true upserts by id and keeps the order of first sight.
        let step = m.on_frame(&json!({"jsonrpc":"2.0","id":1,"method":"cursor/update_todos","params":{"toolCallId":id,
            "todos":[{"id":"verify","content":"verify it","status":"cancelled"},{"id":"inspect","content":"inspect","status":"completed"},{"id":"ship","content":"ship","status":"in_progress"}],"merge":true}}));
        let [AgentEvent::Plan { entries }] = step.events.as_slice() else {
            panic!("{:?}", step.events);
        };
        assert_eq!(
            entries
                .iter()
                .map(|e| (e.content.as_str(), e.status))
                .collect::<Vec<_>>(),
            [
                ("inspect", PlanStatus::Done),
                ("draft", PlanStatus::Todo),
                ("ship", PlanStatus::InProgress)
            ]
        );
        let echoed = &step.outbound[0]["result"]["outcome"]["todos"];
        assert_eq!(
            echoed[2],
            json!({"id":"verify","content":"verify it","status":"cancelled"})
        );
        assert_eq!(echoed.as_array().unwrap().len(), 4);
        // A notification-shaped delivery (the documented form) draws too and
        // gets no reply; the list stays bounded.
        let mut many: Vec<Value> = (0..200)
            .map(|i| json!({"id":format!("t{i}"),"content":"x","status":"pending"}))
            .collect();
        many.push(json!({"id":"","content":"no id","status":"pending"}));
        let step = m.on_frame(&json!({"jsonrpc":"2.0","method":"cursor/update_todos","params":{"toolCallId":id,"todos":many,"merge":false}}));
        assert!(step.outbound.is_empty());
        let [AgentEvent::Plan { entries }] = step.events.as_slice() else {
            panic!("{:?}", step.events);
        };
        assert_eq!(entries.len(), PLAN_TASKS_CAP);
    }

    /// Grok's own plan tool rows (`todo_write`, `_meta["x.ai/tool"].kind ==
    /// "plan"`) draw nothing; its list arrives as the `plan` update, which
    /// still draws the card.
    #[test]
    fn grok_plan_tool_rows_are_hidden_and_the_plan_update_draws() {
        let mut m = mapper();
        m.kind = GROK.kind;
        let id = "call-d033afac-0960-49db-8ea6-4c820319112a-0";
        let opened = m.on_frame(&update(json!({"sessionUpdate":"tool_call","toolCallId":id,"title":"todo_write",
            "rawInput":{"todos":[{"id":"inspect","content":"inspect","status":"in_progress"}],"merge":false},
            "_meta":{"x.ai/tool":{"version":1,"name":"todo_write","kind":"plan","namespace":"grok_build","label":"Plan","read_only":false}}})));
        assert!(opened.events.is_empty(), "{:?}", opened.events);
        let updated = m.on_frame(&update(json!({"sessionUpdate":"tool_call_update","toolCallId":id,"kind":"think","title":"Updating plan","status":"completed"})));
        assert!(updated.events.is_empty() && m.hidden_calls.is_empty());
        let step = m.on_frame(&update(json!({"sessionUpdate":"plan","entries":[{"content":"inspect","priority":"medium","status":"in_progress"},{"content":"draft","priority":"medium","status":"pending"}]})));
        assert!(
            matches!(step.events.as_slice(), [AgentEvent::Plan { entries }] if entries.len() == 2)
        );
        // An ordinary tool row still draws.
        let step = m.on_frame(&update(json!({"sessionUpdate":"tool_call","toolCallId":"call-2","title":"use_tool","rawInput":{"tool_name":"chimaera__notify","tool_input":{}},
            "_meta":{"x.ai/tool":{"kind":"use_tool"}}})));
        assert!(matches!(
            step.events.first(),
            Some(AgentEvent::ToolCall { .. })
        ));
    }
}
