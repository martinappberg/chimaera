import { describe, expect, it } from "vitest";
import type { TimelineEntry } from "./timeline.svelte";
import {
  GROUP_GAP_MS,
  badNewsFirst,
  commandHead,
  commandLabel,
  dayGroups,
  filterGroups,
  filtersPresent,
  formatDuration,
  groupEvidence,
  groupTimeline,
  isBadNews,
  mastermindInbox,
  sinceYouLeft,
} from "./timelineModel";

const T0 = Date.UTC(2026, 8, 25, 12, 0, 0);
const min = (n: number) => n * 60_000;

let seq = 0;
function ep(sid: string, atMin: number, extra: Partial<TimelineEntry> = {}): TimelineEntry {
  return {
    seq: ++seq,
    ts: T0 + min(atMin),
    kind: "episode",
    sid,
    name: sid,
    agent: "claude",
    ui: "chat",
    tier: "protocol",
    title: `prompt ${seq}`,
    end: "finished",
    start_ts: T0 + min(atMin) - min(2),
    ms: min(2),
    ...extra,
  };
}
function cmd(atMin: number, exit: number, ms = min(20)): TimelineEntry {
  return {
    seq: ++seq,
    ts: T0 + min(atMin),
    kind: "command",
    sid: "t1",
    name: "snakemake",
    command: { text: "snakemake -j 32 de_all", exit, ms, source: "user" },
  };
}
function job(atMin: number, state: string): TimelineEntry {
  return { seq: ++seq, ts: T0 + min(atMin), kind: "job", job: { id: "1", name: "align", state } };
}
function know(atMin: number, to: string): TimelineEntry {
  return {
    seq: ++seq,
    ts: T0 + min(atMin),
    kind: "knowledge",
    knowledge: { change: "status", id: "F-002", from: "supported", to, claim: "c" },
  };
}

describe("groupTimeline", () => {
  it("folds consecutive turns of one session within ten minutes into one row", () => {
    seq = 0;
    const a = ep("s1", 0);
    const b = ep("s1", 5);
    const c = ep("s1", 9);
    const groups = groupTimeline([c, a, b]);
    expect(groups).toHaveLength(1);
    expect(groups[0].entries.map((e) => e.seq)).toEqual([a.seq, b.seq, c.seq]);
    expect(groups[0].followUps).toBe(2);
    expect(groups[0].first).toBe(a);
    expect(groups[0].last).toBe(c);
  });

  it("starts a new row past the gap, and per session", () => {
    seq = 0;
    const a = ep("s1", 0);
    const b = ep("s1", 0 + (GROUP_GAP_MS + min(3)) / 60_000);
    const other = ep("s2", 4);
    const groups = groupTimeline([a, b, other]);
    expect(groups.map((g) => g.first.seq)).toEqual([b.seq, other.seq, a.seq]);
    expect(groups.every((g) => g.followUps === 0)).toBe(true);
  });

  it("an entry of another session between two turns does not break the chain", () => {
    seq = 0;
    const a = ep("s1", 0);
    const c = cmd(2, 1);
    const b = ep("s1", 4);
    const groups = groupTimeline([a, c, b]);
    expect(groups).toHaveLength(2);
    const chain = groups.find((g) => g.kind === "episode");
    expect(chain?.entries.map((e) => e.seq)).toEqual([a.seq, b.seq]);
  });

  it("a session crash closes that session's chain", () => {
    seq = 0;
    const a = ep("s1", 0);
    const crash: TimelineEntry = { seq: ++seq, ts: T0 + min(1), kind: "session", sid: "s1", end: "errored" };
    const b = ep("s1", 3);
    const groups = groupTimeline([a, crash, b]);
    expect(groups.filter((g) => g.kind === "episode")).toHaveLength(2);
  });

  it("is newest first", () => {
    seq = 0;
    const a = ep("s1", 0);
    const j = job(30, "COMPLETED");
    const c = cmd(20, 0);
    expect(groupTimeline([a, j, c]).map((g) => g.kind)).toEqual(["job", "command", "episode"]);
  });
});

describe("bad news leads", () => {
  it("classifies failures and contradictions as bad news", () => {
    seq = 0;
    expect(isBadNews(cmd(0, 1))).toBe(true);
    expect(isBadNews(cmd(0, 0))).toBe(false);
    expect(isBadNews(job(0, "FAILED"))).toBe(true);
    expect(isBadNews(job(0, "CANCELLED by 1234"))).toBe(true);
    expect(isBadNews(job(0, "COMPLETED"))).toBe(false);
    expect(isBadNews(know(0, "contradicted"))).toBe(true);
    expect(isBadNews(know(0, "supported"))).toBe(false);
    expect(isBadNews(ep("s1", 0, { end: "errored" }))).toBe(true);
    expect(isBadNews(ep("s1", 0))).toBe(false);
  });

  it("sorts bad rows ahead, each half keeping newest-first", () => {
    seq = 0;
    const good1 = ep("s1", 0);
    const bad1 = cmd(10, 1);
    const good2 = job(20, "COMPLETED");
    const bad2 = know(30, "contradicted");
    const ordered = badNewsFirst(groupTimeline([good1, bad1, good2, bad2]));
    expect(ordered.map((g) => g.first.seq)).toEqual([bad2.seq, bad1.seq, good2.seq, good1.seq]);
  });
});

describe("sinceYouLeft", () => {
  it("shows only entries past the viewer's last look, bad news first, capped with a total", () => {
    seq = 0;
    const old = ep("s1", -60);
    const seen = cmd(-30, 1);
    const baseline = seen.seq;
    const fresh: TimelineEntry[] = [];
    for (let i = 0; i < 10; i++) fresh.push(ep(`s${i}`, i));
    const failure = cmd(11, 1);
    const all = [old, seen, ...fresh, failure];
    const { rows, total } = sinceYouLeft(all, baseline, 8);
    expect(total).toBe(11);
    expect(rows).toHaveLength(8);
    expect(rows[0].first).toBe(failure);
    expect(rows.some((r) => r.first === old || r.first === seen)).toBe(false);
  });

  it("a baseline of zero shows everything", () => {
    seq = 0;
    const a = ep("s1", 0);
    expect(sinceYouLeft([a], 0).total).toBe(1);
  });
});

describe("evidence + durations", () => {
  it("folds files, tools, turns and what got recorded across a group", () => {
    seq = 0;
    const a = ep("s1", 0, {
      evidence: { files: ["a.R", "b.R"], files_n: 2, tools: 3, recorded: { findings: ["F-004"], learnings: 1, decisions: 0 } },
    });
    const b = ep("s1", 3, {
      evidence: { files: ["b.R", "c.R"], files_n: 5, tools: 4, recorded: { findings: [], learnings: 1, decisions: 1 } },
    });
    const [g] = groupTimeline([a, b]);
    const ev = groupEvidence(g);
    expect(ev.files).toEqual(["a.R", "b.R", "c.R"]);
    expect(ev.filesN).toBe(7);
    expect(ev.tools).toBe(7);
    expect(ev.turns).toBe(2);
    expect(ev.recorded).toEqual({ findings: ["F-004"], learnings: 2, decisions: 1 });
  });

  it("formats durations in the dashboard's voice", () => {
    expect(formatDuration(12_000)).toBe("12 s");
    expect(formatDuration(min(38))).toBe("38 min");
    expect(formatDuration(min(133))).toBe("2h 13m");
    expect(formatDuration(min(120))).toBe("2h");
  });

  it("names a command by its program and labels it without env noise", () => {
    expect(commandHead("/usr/bin/snakemake -j 32 de_all")).toBe("snakemake");
    expect(commandHead("  Rscript scripts/de.R")).toBe("Rscript");
    expect(commandHead("FOO=1 BAR=2 python run.py")).toBe("python");
    expect(commandLabel("FOO=1 /opt/bin/snakemake -j 32 de_all")).toBe("snakemake -j 32 de_all");
    expect(commandLabel("x".repeat(60), 10)).toBe("xxxxxxxxx…");
  });
});

describe("filters + days", () => {
  it("offers only filters with rows, and Problems only when something went wrong", () => {
    seq = 0;
    const groups = groupTimeline([ep("s1", 0), job(1, "COMPLETED")]);
    expect(filtersPresent(groups)).toEqual(["all", "agents", "jobs"]);
    const withBad = groupTimeline([ep("s1", 0), cmd(1, 1)]);
    expect(filtersPresent(withBad)).toEqual(["all", "agents", "commands", "problems"]);
    expect(filterGroups(withBad, "problems").map((g) => g.kind)).toEqual(["command"]);
  });

  it("buckets rows by local day, newest first, with Today/Yesterday labels", () => {
    seq = 0;
    const now = T0 + min(60);
    const today = ep("s1", 10);
    const yesterday = ep("s2", -24 * 60);
    const older = ep("s3", -5 * 24 * 60);
    const days = dayGroups(groupTimeline([older, today, yesterday]), now);
    expect(days.map((d) => d.label.split(" ")[0])).toEqual(["Today", "Yesterday", expect.any(String)]);
    expect(days[2].label).not.toBe("Yesterday");
    expect(days.map((d) => d.groups.length)).toEqual([1, 1, 1]);
  });
});

describe("mastermindInbox", () => {
  it("counts unread notes addressed to the Mastermind only", () => {
    seq = 0;
    const toMm: TimelineEntry = {
      seq: ++seq,
      ts: T0,
      kind: "note",
      note: { from_sid: "s1", from_name: "claude-1", to: "mastermind", text: "hi" },
    };
    const toOther: TimelineEntry = {
      seq: ++seq,
      ts: T0,
      kind: "note",
      note: { from_sid: "s1", from_name: "claude-1", to: "s2", text: "hi" },
    };
    const later: TimelineEntry = { ...toMm, seq: ++seq };
    expect(mastermindInbox([later, toOther, toMm], toMm.seq).map((e) => e.seq)).toEqual([later.seq]);
  });
});
