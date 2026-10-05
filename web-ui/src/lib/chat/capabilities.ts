/** The adapter's control contract. A model catalog alone does not prove that
 * model switching, reasoning controls, or image input are implemented. */
export interface ChatCapabilities {
  commands: string[];
  image_input: boolean;
  /** Accepts a model ID outside the reported catalog. Never inferred from set_model. */
  custom_model: boolean;
}

const shared = [
  "send", "permission", "permission_feedback", "interrupt", "set_mode", "set_model", "set_effort",
  "answer", "get_usage", "cancel_queued", "send_after_turn", "send_now", "send_if_running",
];

/** Only for journals/daemons predating the capability snapshot. New harnesses
 * start with no optional controls until their adapter reports them. */
export function legacyCapabilities(agent: string): ChatCapabilities {
  const extra = agent === "claude"
    ? ["permission_destination", "set_thinking", "set_ultracode", "rewind", "background_tool", "stop_task",
       "get_mcp", "set_mcp_enabled", "reconnect_mcp", "set_remote_control"]
    : agent === "codex" ? ["compact", "steer_queued"] : null;
  return { commands: extra === null ? [] : [...shared, ...extra], image_input: extra !== null, custom_model: false };
}

export function parseCapabilities(value: unknown): ChatCapabilities {
  const raw = value !== null && typeof value === "object" ? value as Record<string, unknown> : {};
  return {
    commands: Array.isArray(raw.commands)
      ? [...new Set(raw.commands.filter((c): c is string => typeof c === "string" && c.length <= 64))].slice(0, 64)
      : [],
    image_input: raw.image_input === true,
    custom_model: raw.custom_model === true,
  };
}
