# Agent protocol contract: index

The agent wire formats are not stable contracts. Claude Code and Codex are unversioned; ACP negotiates v1, but agent behavior still drifts. These files record what we know, how we know it, and what we have not adopted.

Sources: **live** = probed against the real CLI at the version named; **official schema** = Codex's `codex app-server generate-ts` / `generate-json-schema` output for that version, plus upstream docs; **vsix** = mined from the official VS Code extensions (Anthropic.claude-code 2.1.204, openai.chatgpt 26.5623.141536). Every fact carries its pass and CLI version, for example "(Pass 31, claude 2.1.281)".

## Providers

| Provider | Driver | Pin (current value) | Live gate | Reference | Latest pass |
|---|---|---|---|---|---|
| Claude Code stream-json | `src/claude.rs` | `TESTED_CLAUDE_VERSION` = 2.1.310 | `just chat-smoke` | [claude-code.md](protocol/claude-code.md) | Pass 53 |
| Codex app-server | `src/codex.rs` | `TESTED_CODEX_VERSION` = 0.164.0 | `just chat-smoke` | [codex.md](protocol/codex.md) | Pass 54 |
| ACP: Grok Build | `src/acp.rs` | `TESTED_GROK_VERSION` = 1.0.46 | `just chat-smoke-acp` | [acp.md](protocol/acp.md) | Pass 46 addendum |
| ACP: Google Antigravity | `src/acp.rs` | `TESTED_AGY_VERSION` = 1.2.1 | `just chat-smoke-acp` | [acp.md](protocol/acp.md) | Pass 46 addendum |
| ACP: Cursor CLI | `src/acp.rs` | `TESTED_CURSOR_VERSION` = 2026.10.14-941492e | `just chat-smoke-acp` | [acp.md](protocol/acp.md) | Pass 46 addendum |
| Claude Mods native UI | `src/native_ui.rs` | `TESTED_CLAUDE_VERSION` | `just chat-mods-smoke` | [mods.md](protocol/mods.md) | Pass 52 |

The shared rules are in [cross-agent.md](protocol/cross-agent.md). The live gates bill turns, except `chat-mods-smoke`.

## Cross-agent contract

- One normalized, ACP-shaped event model in `model.rs`; every driver translates into it, and caps apply at event construction ([invariants](protocol/cross-agent.md#cross-agent-invariants)).
- An unknown agent→client request never becomes a permission card; the user hears about it once per kind ([invariants](protocol/cross-agent.md#cross-agent-invariants)).
- The version comes from `--version`, not the wire; drift warns and never blocks ([version detection](protocol/cross-agent.md#version-detection)).
- Every ask gets a definitive outcome, and teardown drains pending asks ([ask lifecycle](protocol/cross-agent.md#ask-lifecycle)).
- A mid-turn message is read at the agent's next step; Stop never discards waiting messages; `dropped` means undeliverable ([delivery](protocol/cross-agent.md#user-message-delivery)).
- Stop always recovers: an interrupt watchdog closes a wedged turn, and every stop SIGTERMs before it closes stdin ([watchdog](protocol/cross-agent.md#interrupt-watchdog), [teardown](protocol/cross-agent.md#stop-and-teardown)).

## Recording a new wire fact

1. Write the fact in its topic file first, in present tense, in the section that owns it. Tag it with the pass and the CLI version where you saw it. Keep one home per fact.
2. If the fact replaces an older one, move the older one out of the topic file. Its pass entry keeps the history.
3. Add a dated `## Pass N (date — what you probed)` entry at the end of [protocol/passes.md](protocol/passes.md): what you probed, what you found, what you adopted, and the gate you ran.
4. Do not change an older fact's version tag unless you verified it again.

`Pass N` references in code and docs resolve to [protocol/passes.md](protocol/passes.md). Older references to a section by name resolve to the matching heading: "Version detection" is in cross-agent.md; "ACP › Cursor" is "Cursor CLI over ACP" in passes.md and the Cursor facts in acp.md.

## See also

- [AGENTS.md](AGENTS.md): the crate map and its invariants.
- [agent-protocol rules](../../.claude/rules/agent-protocol.md): the pinned-protocol and live-gate rules.
