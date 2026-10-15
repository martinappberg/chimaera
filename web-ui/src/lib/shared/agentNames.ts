/**
 * Built-in agent product names, keyed by the daemon's agent id. The daemon
 * catalog (`GET /api/v1/agents` → `name`) is the authority; this is the
 * fallback for surfaces that paint before it lands, or for records of an
 * agent the catalog no longer lists. Each vendor's own product name.
 */
export const AGENT_PRODUCT_NAMES: Readonly<Record<string, string>> = {
  claude: "Claude Code",
  codex: "Codex",
  agy: "Antigravity",
  grok: "Grok Build",
  cursor: "Cursor CLI",
};

/** The product name for an agent id; unknown ids (extension agents) stay verbatim. */
export function agentProductName(id: string): string {
  return Object.prototype.hasOwnProperty.call(AGENT_PRODUCT_NAMES, id) ? AGENT_PRODUCT_NAMES[id] : id;
}
