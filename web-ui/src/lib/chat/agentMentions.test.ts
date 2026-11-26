import { describe, expect, it } from "vitest";
import {
  agentLabel,
  agentMatches,
  agentMentions,
  agentTagTargets,
  collapseAgentMentions,
  mentionChoices,
  mentionedAgent,
  type AgentOption,
} from "./agentMentions";
import { agentRefPanes } from "./agentRoster";
import { expandUploadMentions, snapRange, tokenSpans } from "./uploadTokens";

const agent = (id: string, name: string, more: Partial<AgentOption> = {}): AgentOption => ({
  id,
  name,
  agentKind: "claude",
  workspaceId: "w-1",
  alive: true,
  ...more,
});
const agents = [
  agent("s-1111aaaa", "analyst"),
  agent("s-2222bbbb", "reviewer", { agentKind: "codex" }),
  agent("s-3333cccc", "reviewer"),
  agent("s-4444dddd", "old one", { alive: false }),
  agent("s-5555eeee", "elsewhere", { workspaceId: "w-2" }),
];

describe("agent mentions", () => {
  it("reads ids, bare names and quoted names, leaving sentence punctuation out", () => {
    const text = `ask (@agent:s-1111aaaa), then @agent:"old one" and @agent:“old one”, not @agent: nothing.`;
    const refs = agentMentions(text);
    expect(refs.map((r) => r.target)).toEqual(["s-1111aaaa", "old one", "old one"]);
    expect(refs.map((r) => text.slice(r.start, r.end))).toEqual([
      "@agent:s-1111aaaa",
      '@agent:"old one"',
      "@agent:“old one”",
    ]);
  });

  it("names an agent by id, or by a name only one live agent has", () => {
    expect(mentionedAgent("s-2222bbbb", agents)).toBe(agents[1]);
    expect(mentionedAgent("Analyst", agents)).toBe(agents[0]);
    // Two reviewers: never a guess.
    expect(mentionedAgent("reviewer", agents)).toBeUndefined();
    // An ended agent is found by its id (a sent message keeps its chip), not by name.
    expect(mentionedAgent("s-4444dddd", agents)).toBe(agents[3]);
    expect(mentionedAgent("old one", agents)).toBeUndefined();
  });

  it("tells same-named agents apart on the chip", () => {
    expect(agentLabel(agents[0], agents)).toBe("analyst");
    expect(agentLabel(agents[1], agents)).toBe("reviewer · 2222bbbb");
    expect(agentLabel(agents[2], agents)).toBe("reviewer · 3333cccc");
  });

  it("offers a chat the live agents of its workspace, never itself", () => {
    expect(mentionChoices(agents, "s-1111aaaa", "w-1").map((a) => a.id)).toEqual(["s-2222bbbb", "s-3333cccc"]);
    expect(agentMatches(mentionChoices(agents, "s-1111aaaa", "w-1"), "codex").map((a) => a.id)).toEqual(["s-2222bbbb"]);
    expect(agentMatches(mentionChoices(agents, null, "w-1"), "").map((a) => a.id)).toEqual([
      "s-1111aaaa",
      "s-2222bbbb",
      "s-3333cccc",
    ]);
  });

  it("shows a readable chip while the send, the copy and the saved draft keep the id", () => {
    const tokens = new Map<string, string>();
    const text = "ask @agent:s-1111aaaa and @agent:s-2222bbbb, then stop.";
    const shown = collapseAgentMentions(text, agents, tokens);
    expect(shown).toBe("ask @agent:“analyst” and @agent:“reviewer · 2222bbbb”, then stop.");
    expect(expandUploadMentions(shown, tokens)).toBe(text);
    const chip = tokenSpans(shown, tokens)[0];
    expect(snapRange(tokenSpans(shown, tokens), chip.start + 2, chip.end - 2)).toEqual([chip.start, chip.end]);
    // The same text collapses the same way again (a remounted composer).
    expect(collapseAgentMentions(text, agents, new Map())).toBe(shown);
  });

  it("leaves a typed name and an unknown id as written", () => {
    const tokens = new Map<string, string>();
    const text = "ask @agent:analyst and @agent:s-00000000";
    expect(collapseAgentMentions(text, agents, tokens)).toBe(text);
    expect(tokens.size).toBe(0);
  });

  it("never takes a name typed by hand for an earlier pick", () => {
    const tokens = new Map<string, string>();
    const shown = collapseAgentMentions("@agent:s-1111aaaa", agents, tokens);
    expect(tokenSpans(`${shown} and @agent:analyst`, tokens)).toHaveLength(1);
    expect(expandUploadMentions("see @agent:analyst", tokens)).toBe("see @agent:analyst");
  });

  it("keeps a chip apart from the same words already in the draft", () => {
    const tokens = new Map<string, string>();
    const typed = "@agent:“analyst”";
    const shown = collapseAgentMentions("@agent:s-1111aaaa", agents, tokens, typed);
    expect(shown).not.toBe(typed);
    expect(expandUploadMentions(`${typed} ${shown}`, tokens)).toBe(`${typed} @agent:s-1111aaaa`);
  });

  it("tags an agent's id in prose, written as a mention or bare, and nothing else", () => {
    const [tag] = agentTagTargets("@agent:s-1111aaaa", agents);
    expect(tag).toMatchObject({ key: "s-1111aaaa", kind: "agent", label: "analyst" });
    expect(tag.icon?.d).toBeTruthy();
    expect(agentTagTargets("s-2222bbbb", agents)[0].label).toBe("reviewer · 2222bbbb");
    // An ended agent keeps its name; a session that is no agent here stays text.
    expect(agentTagTargets("s-4444dddd", agents)[0].label).toBe("old one");
    expect(agentTagTargets("s-00000000", agents)).toEqual([]);
    expect(agentTagTargets("@agent:analyst", agents)).toEqual([]);
  });

  it("leaves a session id alone where it is part of a path or a file name", () => {
    const site = (text: string) => ({ text, index: text.indexOf("s-1111aaaa") });
    for (const text of [
      "read /home/u/.chimaera/uploads/s-1111aaaa/shot.png",
      "see run/agents/s-1111aaaa-settings.json",
      "wrote s-1111aaaa.log",
      "in uploads/s-1111aaaa",
    ]) {
      // (the second one never reaches the source: the matcher's own edges stop at the dash)
      if (text.includes("-settings")) continue;
      expect(agentTagTargets("s-1111aaaa", agents, site(text)), text).toEqual([]);
    }
    // In a sentence it is the agent; the mention form is the agent wherever it stands.
    expect(agentTagTargets("s-1111aaaa", agents, site("I asked s-1111aaaa. It agreed"))).toHaveLength(1);
    expect(agentTagTargets("s-1111aaaa", agents, site("(s-1111aaaa)"))).toHaveLength(1);
    const mention = "see /x/@agent:s-1111aaaa/";
    expect(agentTagTargets("@agent:s-1111aaaa", agents, { text: mention, index: mention.indexOf("@") })).toHaveLength(1);
  });

  it("references a dragged agent only in another live agent's pane of its workspace", () => {
    const shown = [
      { paneId: "p-self", sessionId: "s-1111aaaa" },
      { paneId: "p-peer", sessionId: "s-2222bbbb" },
      { paneId: "p-ended", sessionId: "s-4444dddd" },
      { paneId: "p-other-ws", sessionId: "s-5555eeee" },
      { paneId: "p-shell", sessionId: "s-9999ffff" },
    ];
    expect([...(agentRefPanes("s-1111aaaa", shown, agents) ?? [])]).toEqual(["p-peer"]);
    // Not a live agent of this window: the drag is an ordinary tab move.
    expect(agentRefPanes("s-9999ffff", shown, agents)).toBeUndefined();
    expect(agentRefPanes("s-4444dddd", shown, agents)).toBeUndefined();
  });

  it("never reassigns a chip when another agent takes the name", () => {
    const tokens = new Map<string, string>();
    const first = collapseAgentMentions("@agent:s-1111aaaa", agents, tokens);
    const second = collapseAgentMentions("@agent:s-9999ffff", [agent("s-9999ffff", "analyst")], tokens);
    expect(first).not.toBe(second);
    expect(expandUploadMentions(first, tokens)).toBe("@agent:s-1111aaaa");
    expect(expandUploadMentions(second, tokens)).toBe("@agent:s-9999ffff");
  });
});
