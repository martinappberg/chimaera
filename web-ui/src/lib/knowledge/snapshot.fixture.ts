/**
 * A small knowledge snapshot in the 0.2 wire shape for the pure tests:
 * a reused finding id across two topics, a correction, prose and template
 * findings, both decision eras, to-dos from a table and a section, a
 * handoff, asks and tidy rows. Test-only (never imported by the app).
 */
import { normalizeKnowledge, type Knowledge } from "../workspace/knowledge";

export const RAW = {
  schema: 1,
  provider: "sample",
  left_off: {
    worked_on: "Ran the stage 08 audit.",
    decisions: "",
    blockers: ["Waiting on the grouping vote"],
    current: "Freeze prep for stage 08.",
    next: ["Put the vote to the user (F-228)", "Apply D-157"],
    written_ms: 1_790_000_000_000,
    path: ".mycelium/run/claude/abc/last-session.md",
    session_id: "abc",
    host: "claude",
    span: { path: ".mycelium/run/claude/abc/last-session.md", line: 1, end_line: 20 },
    sources: [
      { path: ".mycelium/run/claude/abc/last-session.md", written_ms: 1_790_000_000_000, session_id: "abc", host: "claude" },
      { path: ".mycelium/last-session.md", written_ms: 1_789_900_000_000 },
    ],
  },
  topics: [
    {
      slug: "depth",
      description: "Sequencing depth",
      path: ".living/findings/depth.md",
      date: "2026-09-28",
      findings: [
        {
          id: "F-228",
          key: "depth/F-228",
          claim: "Reproducibility rises smoothly with depth",
          status: "unknown",
          stated: "",
          date: "2026-09-28",
          implications: "",
          tags: [],
          ledger: [],
          questions: [],
          line: 40,
          updated: "2026-09-28",
          span: { path: ".living/findings/depth.md", line: 40, end_line: 60 },
          refs: [
            { kind: "finding", id: "F-227" },
            { kind: "decision", id: "D-152" },
          ],
          cites: [
            { kind: "script", text: "r08_saturation.py" },
            { kind: "job", text: "52645539" },
          ],
        },
        {
          id: "F-227",
          key: "depth/F-227",
          claim: "Three groupings are close",
          status: "unknown",
          stated: "",
          date: "2026-09-27",
          line: 10,
          updated: "2026-09-27",
          span: { path: ".living/findings/depth.md", line: 10, end_line: 38 },
        },
      ],
    },
    {
      slug: "onboarding",
      description: "Onboarding each cohort",
      path: ".living/findings/onboarding.md",
      findings: [
        {
          id: "F-177",
          key: "onboarding/F-177",
          claim: "The reference is mixed",
          status: "unknown",
          stated: "established by independent realignment",
          date: "2026-09-03",
          line: 80,
          updated: "2026-09-03",
          span: { path: ".living/findings/onboarding.md", line: 80, end_line: 95 },
          refs: [{ kind: "finding", id: "F-171" }],
          amends: [{ kind: "corrects", id: "F-171" }],
        },
        {
          id: "F-171",
          key: "onboarding/F-171",
          claim: "The reference is uniform",
          status: "supported",
          stated: "supported (provider manifest)",
          date: "2026-08-29",
          line: 60,
          updated: "2026-08-29",
          ledger: [{ date: "2026-08-29", run: "r1", dataset: "d", project: "p", result: "ok", direction: "supports" }],
          span: { path: ".living/findings/onboarding.md", line: 60, end_line: 78 },
          state: { kind: "corrected", by: "F-177" },
        },
      ],
    },
    {
      slug: "qc",
      description: "Per-cell QC",
      path: ".living/findings/qc.md",
      findings: [
        {
          id: "F-177",
          key: "qc/F-177",
          claim: "The gate was measured on its own output",
          status: "unknown",
          stated: "",
          date: "2026-09-02",
          line: 5,
          updated: "2026-09-02",
          span: { path: ".living/findings/qc.md", line: 5, end_line: 12 },
          refs: [{ kind: "finding", id: "F-171" }],
        },
      ],
    },
  ],
  decisions: [
    {
      fp: "D-aaa",
      id: "D-157",
      date: "2026-09-28",
      title: "The null sets each pair's bar",
      stated: "DECIDED (agent)",
      line: 900,
      span: { path: ".living/decisions.md", line: 900, end_line: 920 },
      refs: [{ kind: "decision", id: "D-152" }],
    },
    {
      fp: "D-bbb",
      id: "D-152",
      date: "2026-09-26",
      title: "The null is a gate",
      line: 880,
      span: { path: ".living/decisions.md", line: 880, end_line: 898 },
      state: { kind: "superseded", by: "D-157" },
    },
  ],
  learnings: [
    {
      fp: "L-aaa",
      date: "2026-09-27",
      title: "The strongest signature can be the worst population",
      category: "gotcha",
      line: 5,
      span: { path: ".living/learnings.md", line: 5, end_line: 14 },
    },
  ],
  todos: [
    {
      key: "todo/T-Chaining",
      item: "Audit merges for chaining. Replay 20 units.",
      title: "Audit merges for chaining.",
      priority: "critical",
      status: "in-progress",
      id: "T-Chaining",
      closed: false,
      source: "section",
      span: { path: "todo/TODO_REGISTRY.md", line: 90, end_line: 99 },
    },
    {
      key: "todo/r1",
      item: "Liftover and phasing",
      priority: "high",
      status: "done 2026-09-23 (applied)",
      closed: true,
      source: "table",
      span: { path: "todo/TODO_REGISTRY.md", line: 30, end_line: 30 },
    },
    {
      key: "todo/r2",
      item: "Chromatin tracks from the roster",
      priority: "high",
      status: "blocked",
      closed: false,
      source: "table",
    },
    { key: "todo/r3", item: "Publication figures", priority: "medium", status: "open", closed: false, source: "table" },
  ],
  questions: [{ text: "Class-aware thresholds?", finding: "F-171", key: "onboarding/F-171" }],
  conventions: [{ key: "conventions/C-12", id: "C-12", title: "Stages write versioned folders", status: "" }],
  sessions: [{ id: "2026-09-28-004", date: "2026-09-28", summary: "Freeze prep", log: ".living/log/2026-09-28-004.md" }],
  asks: [
    {
      text: "Put to the user with the vote: one rerun?",
      date: "2026-09-28",
      source: { kind: "finding", id: "F-228", key: "depth/F-228" },
      span: { path: ".living/findings/depth.md", line: 58, end_line: 58 },
    },
    { text: "Parked: stratify the split?", date: "2026-09-28", source: { kind: "handoff", id: "", key: "" } },
  ],
  tidy: [
    {
      kind: "duplicate-id",
      text: "F-177 names two findings",
      refs: [{ kind: "finding", id: "F-177" }],
      ask: "Renumber the later F-177.",
    },
  ],
  id_shapes: [
    { kind: "finding", pattern: "F-\\d{1,4}" },
    { kind: "decision", pattern: "D-\\d{1,4}" },
    { kind: "convention", pattern: "C-\\d{1,3}" },
    { kind: "todo", pattern: "T-[A-Za-z][A-Za-z0-9]*" },
  ],
  labels: {
    source: "sample · read-only",
    sections: { learnings: "Watch out for" },
    kinds: { todo: "to-do" },
    status_words: [
      { word: "supported", rank: 2, tone: "good" },
      { word: "contradicted", rank: 0, tone: "bad" },
    ],
    status_note: "Written by the agent.",
  },
  counts: { findings: 5, decisions: 2, learnings: 1, open: 3 },
  guidance: [],
  warnings: [],
};

export function sample(): Knowledge {
  return normalizeKnowledge(structuredClone(RAW));
}
