/**
 * Pure helpers for the cluster surfaces (the host row, the cluster page, the
 * start sheet, the job window's notices): workspace state → words, the start
 * sheet's form → a Slurm launch spec, and time formatting.
 *
 * Generic Slurm only — nothing here knows a site, a partition name, or a
 * site command. Slurm's own words (a pending reason, a refusal message) are
 * shown verbatim; only the terminal states a job ends in are put into plain
 * words, because "TIMEOUT" answers "why did it stop?" worse than "hit its
 * time limit" does.
 */
import type {
  ClusterConfig,
  ClusterFacts,
  ClusterOverview,
  ClusterWorkspaceView,
  LaunchSpec,
  PartitionChoice,
  StartResult,
} from "../net/native";
import { hostSummary, schedulerLabel, shortDuration, timeLeftWords } from "./clusterRow";

export { hostSummary, schedulerLabel, shortDuration, timeLeftWords };

const MINUTE = 60;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

// --- durations and times ------------------------------------------------------

function plural(n: number, one: string, many: string): string {
  return `${n} ${n === 1 ? one : many}`;
}

/** A limit in words: "7 days", "2 days 12 hours", "12 hours", "30 minutes". */
export function limitWords(totalSecs: number): string {
  const secs = Math.max(0, Math.floor(totalSecs));
  const d = Math.floor(secs / DAY);
  const h = Math.floor((secs % DAY) / HOUR);
  const m = Math.floor((secs % HOUR) / MINUTE);
  const parts: string[] = [];
  if (d > 0) parts.push(plural(d, "day", "days"));
  if (h > 0) parts.push(plural(h, "hour", "hours"));
  // Minutes only matter when the limit is short or not a whole hour count.
  if (m > 0 && d === 0) parts.push(plural(m, "minute", "minutes"));
  return parts.length === 0 ? "under a minute" : parts.join(" ");
}

/** "just now", "5 min ago", "3 h ago", "yesterday", "4 days ago", then a date. */
export function agoWords(thenMs: number, nowMs: number, locale?: string): string {
  const secs = Math.max(0, Math.floor((nowMs - thenMs) / 1000));
  if (secs < MINUTE) return "just now";
  if (secs < HOUR) return `${Math.floor(secs / MINUTE)} min ago`;
  if (secs < DAY) return `${Math.floor(secs / HOUR)} h ago`;
  if (secs < 2 * DAY) return "yesterday";
  if (secs < 14 * DAY) return `${Math.floor(secs / DAY)} days ago`;
  return new Intl.DateTimeFormat(locale, { month: "short", day: "numeric" }).format(
    new Date(thenMs),
  );
}

function sameLocalDay(a: Date, b: Date): boolean {
  return (
    a.getFullYear() === b.getFullYear() &&
    a.getMonth() === b.getMonth() &&
    a.getDate() === b.getDate()
  );
}

/** A future moment as a clock: "14:20", "tomorrow 09:00", "Mon 14:20", "Oct 3, 14:20". */
export function clockWords(atMs: number, nowMs: number, locale?: string): string {
  const at = new Date(atMs);
  const now = new Date(nowMs);
  const time = new Intl.DateTimeFormat(locale, { timeStyle: "short" }).format(at);
  if (sameLocalDay(at, now)) return time;
  const tomorrow = new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1);
  if (sameLocalDay(at, tomorrow)) return `tomorrow ${time}`;
  if (atMs > nowMs && atMs - nowMs < 6 * DAY * 1000) {
    const day = new Intl.DateTimeFormat(locale, { weekday: "short" }).format(at);
    return `${day} ${time}`;
  }
  const date = new Intl.DateTimeFormat(locale, { month: "short", day: "numeric" }).format(at);
  return `${date}, ${time}`;
}

// --- Slurm walltime ------------------------------------------------------------

/**
 * A Slurm `--time` value → seconds. Accepts Slurm's input forms: `M`, `M:S`,
 * `H:M:S`, `D-H`, `D-H:M`, `D-H:M:S`. Null for anything else (`UNLIMITED`,
 * `INFINITE`, blanks).
 */
export function parseSlurmTime(raw: string): number | null {
  const s = raw.trim();
  let m = s.match(/^(\d+)-(\d+)(?::(\d+))?(?::(\d+))?$/);
  if (m !== null) {
    return (
      Number(m[1]) * DAY +
      Number(m[2]) * HOUR +
      Number(m[3] ?? 0) * MINUTE +
      Number(m[4] ?? 0)
    );
  }
  m = s.match(/^(\d+)(?::(\d+))?(?::(\d+))?$/);
  if (m === null) return null;
  if (m[3] !== undefined) return Number(m[1]) * HOUR + Number(m[2]) * MINUTE + Number(m[3]);
  if (m[2] !== undefined) return Number(m[1]) * MINUTE + Number(m[2]);
  return Number(m[1]) * MINUTE;
}

/** Seconds → Slurm `--time`: `HH:MM:SS`, or `D-HH:MM:SS` from a day up. "" for 0. */
export function composeWalltime(totalSecs: number): string {
  const secs = Math.max(0, Math.floor(totalSecs));
  if (secs === 0) return "";
  const d = Math.floor(secs / DAY);
  const pad = (n: number) => String(n).padStart(2, "0");
  const hms = `${pad(Math.floor((secs % DAY) / HOUR))}:${pad(Math.floor((secs % HOUR) / MINUTE))}:${pad(secs % MINUTE)}`;
  return d > 0 ? `${d}-${hms}` : hms;
}

/** The d/h/m boxes. Numeric inputs bind an empty box as null (read as 0). */
export interface Walltime {
  days: number | null;
  hours: number | null;
  mins: number | null;
}

/** The boxes' total in seconds (negative or fractional entries floor to whole units ≥ 0). */
export function walltimeSecs(w: Walltime): number {
  const n = (v: number | null) => (v === null || !Number.isFinite(v) ? 0 : Math.max(0, Math.floor(v)));
  return ((n(w.days) * 24 + n(w.hours)) * 60 + n(w.mins)) * 60;
}

/** Seconds → the boxes (seconds below a minute round up so a limit is never shortened). */
export function splitWalltime(totalSecs: number): Walltime {
  const mins = Math.ceil(Math.max(0, totalSecs) / MINUTE);
  return {
    days: Math.floor(mins / (24 * 60)),
    hours: Math.floor((mins % (24 * 60)) / 60),
    mins: mins % 60,
  };
}

// --- memory and resources ----------------------------------------------------

/** Slurm's memory spelling → words: "16G" → "16 GB", "4096M" → "4 GB", "4000M" → "4000 MB". */
export function memWords(mem: string | undefined | null): string {
  if (mem === undefined || mem === null) return "";
  const s = mem.trim();
  if (s === "") return "";
  const m = s.match(/^(\d+(?:\.\d+)?)([KMGTP]?)(?:i?B)?$/i);
  if (m === null) return s;
  const n = Number(m[1]);
  const unit = (m[2] === "" ? "M" : m[2]).toUpperCase(); // a bare number is MB to Slurm
  if (unit === "M" && n >= 1024 && n % 1024 === 0) return `${n / 1024} GB`;
  if (unit === "G" && n >= 1024 && n % 1024 === 0) return `${n / 1024} TB`;
  return `${m[1]} ${unit}B`;
}

/** "4 CPU · 16 GB · 1 GPU" — whatever the job carries. */
export function resourceWords(w: Pick<ClusterWorkspaceView, "cpus" | "mem" | "gpus">): string {
  const parts: string[] = [];
  if (w.cpus !== undefined && w.cpus.trim() !== "") parts.push(`${w.cpus.trim()} CPU`);
  const mem = memWords(w.mem);
  if (mem !== "") parts.push(mem);
  if (w.gpus !== undefined && w.gpus > 0) parts.push(w.gpus === 1 ? "1 GPU" : `${w.gpus} GPUs`);
  return parts.join(" · ");
}

// --- workspace state → words --------------------------------------------------

/**
 * Why a workspace's last job ended, in words. `stoppedByUser` wins: the app
 * recorded the stop, so it never guesses a reason over a known one. Slurm's
 * state may carry a suffix (`CANCELLED by 1234`, `CANCELLED+`); the shell
 * reports "stopped" for a stop made from the app.
 */
export function endedWords(ended: string | undefined | null, stoppedByUser: boolean): string {
  if (stoppedByUser) return "stopped by you";
  const state = (ended ?? "").trim().toUpperCase().split(/[\s+]/)[0] ?? "";
  switch (state) {
    case "STOPPED":
      return "stopped by you";
    case "TIMEOUT":
      return "hit its time limit";
    case "CANCELLED":
      return "cancelled";
    case "FAILED":
      return "failed";
    case "PREEMPTED":
      return "preempted";
    case "NODE_FAIL":
      return "its node failed";
    case "OUT_OF_MEMORY":
      return "ran out of memory";
    default:
      return "ended";
  }
}

/** The state dot's class, in the home screen's dot language. */
export function stateDot(state: ClusterWorkspaceView["state"]): "alive" | "booting" | "queued" | "" {
  switch (state) {
    case "running":
      return "alive";
    case "starting":
      return "booting";
    case "waiting":
      return "queued";
    default:
      return "";
  }
}

/** The one line of detail under a workspace on the cluster page. */
export function workspaceDetail(
  w: ClusterWorkspaceView,
  nowMs: number,
  locale?: string,
): string {
  switch (w.state) {
    case "running": {
      const parts: string[] = [w.node ? `running on ${w.node}` : "running"];
      const res = resourceWords(w);
      if (res !== "") parts.push(res);
      if (w.ends_at_ms !== undefined) parts.push(timeLeftWords(w.ends_at_ms, nowMs));
      return parts.join(" · ");
    }
    case "starting":
      return w.node ? `starting chimaera on ${w.node}…` : "starting chimaera…";
    case "waiting": {
      let line = "waiting for a node";
      if (w.start_estimate_ms !== undefined && w.start_estimate_ms !== null) {
        line += ` · Slurm estimates ${clockWords(w.start_estimate_ms, nowMs, locale)}`;
      }
      if (w.reason !== undefined && w.reason.trim() !== "") {
        line += ` · Slurm's reason: ${w.reason.trim()}`;
      }
      return line;
    }
    case "stopped": {
      if (w.fresh) return "not started yet";
      let line = endedWords(w.ended, w.stopped_by_user);
      if (w.ended_at_ms !== undefined) line += ` ${agoWords(w.ended_at_ms, nowMs, locale)}`;
      return `${line} · chats saved`;
    }
  }
}

/** Quiet extra lines a workspace may carry (each one fact, no actions). */
export function workspaceNotes(w: ClusterWorkspaceView): { text: string; warn: boolean }[] {
  const notes: { text: string; warn: boolean }[] = [];
  if (w.state === "running" && w.egress === false) {
    notes.push({ text: "agents can't reach the internet from this node", warn: true });
  }
  if (w.attached && w.state !== "stopped") {
    notes.push({ text: "stops when you disconnect", warn: false });
  }
  return notes;
}

/** "Your other jobs on this cluster: 2 running · 9 waiting" (count only). */
export function otherJobsWords(other: ClusterOverview["other_jobs"]): string {
  if (other.running === 0 && other.waiting === 0) return "Your other jobs on this cluster: none";
  return `Your other jobs on this cluster: ${other.running} running · ${other.waiting} waiting`;
}

// --- the start sheet ------------------------------------------------------------

/** The start sheet's editable fields (text inputs stay strings until validated). */
export interface StartForm extends Walltime {
  /** "" = the cluster's default partition. */
  partition: string;
  cpus: string;
  mem: string;
  gpus: string;
  account: string;
  qos: string;
  constraint: string;
}

export type StartField =
  | "partition"
  | "time"
  | "cpus"
  | "mem"
  | "gpus"
  | "account"
  | "qos"
  | "constraint";

/** Two hours: a first start's time, short enough to queue well anywhere. */
export const DEFAULT_TIME_SECS = 2 * HOUR;

/** A fresh form: the cluster's default partition, two hours, everything else blank. */
export function defaultForm(facts: ClusterFacts | null): StartForm {
  const partition =
    facts?.partitions.find((p) => p.default && p.up)?.name ??
    facts?.partitions.find((p) => p.default)?.name ??
    "";
  return {
    partition,
    ...splitWalltime(DEFAULT_TIME_SECS),
    cpus: "",
    mem: "",
    gpus: "",
    account: facts?.default_account ?? "",
    qos: "",
    constraint: "",
  };
}

/** A saved spec back into the form (a setup chip, or the last run). */
export function formFromSpec(spec: LaunchSpec, facts: ClusterFacts | null): StartForm {
  const secs = parseSlurmTime(spec.time);
  return {
    partition: spec.partition ?? "",
    ...splitWalltime(secs ?? DEFAULT_TIME_SECS),
    cpus: spec.cpus !== undefined && spec.cpus !== null ? String(spec.cpus) : "",
    mem: spec.mem ?? "",
    gpus: spec.gpus !== undefined && spec.gpus !== null && spec.gpus > 0 ? String(spec.gpus) : "",
    account: spec.account ?? facts?.default_account ?? "",
    qos: spec.qos ?? "",
    constraint: spec.constraint ?? "",
  };
}

/** Accounts the account select offers for a partition (its own list when it has one). */
export function accountChoices(facts: ClusterFacts | null, partition: PartitionChoice | null): string[] {
  if (facts === null) return [];
  const own = partition?.accounts ?? [];
  return own.length > 0 ? own : facts.accounts;
}

/** The partition learned to refuse batch jobs (a job there runs attached). */
export function isInteractiveOnly(config: ClusterConfig | null, partition: string): boolean {
  return partition !== "" && (config?.learned.interactive_only ?? []).includes(partition);
}

/** The field a refusal is about, so the sheet can reveal and focus it. */
export function refusalField(
  refusal: Extract<StartResult, { kind: "refused" }>["refusal"],
): "account" | "qos" | "constraint" | null {
  switch (refusal) {
    case "account_required":
      return "account";
    case "qos_required":
      return "qos";
    case "constraint_required":
      return "constraint";
    default:
      return null;
  }
}

/**
 * Validate the form and compose the launch spec. Errors are per field and
 * worded for the line under it; a spec comes back only when there are none.
 * Blank optional fields are omitted (the cluster's default applies).
 */
export function buildSpec(
  form: StartForm,
  ctx: { partition: PartitionChoice | null; requires: readonly string[] },
): { spec: LaunchSpec | null; errors: Partial<Record<StartField, string>> } {
  const errors: Partial<Record<StartField, string>> = {};
  const spec: LaunchSpec = { time: "" };

  const secs = walltimeSecs(form);
  if (secs === 0) {
    errors.time = "Set a time limit — every job needs one.";
  } else if (
    ctx.partition !== null &&
    ctx.partition.max_time_secs !== null &&
    secs > ctx.partition.max_time_secs
  ) {
    errors.time = `${ctx.partition.name} allows up to ${limitWords(ctx.partition.max_time_secs)}.`;
  } else {
    spec.time = composeWalltime(secs);
  }

  const partition = form.partition.trim();
  if (partition !== "") spec.partition = partition;

  const cpus = form.cpus.trim();
  if (cpus !== "") {
    if (!/^\d+$/.test(cpus) || Number(cpus) < 1) errors.cpus = "A whole number, 1 or more.";
    else spec.cpus = Number(cpus);
  }

  const mem = form.mem.trim();
  if (mem !== "") {
    const m = mem.match(/^(\d+)\s*([KMGT])(?:i?B)?$/i);
    if (m !== null && Number(m[1]) > 0) {
      spec.mem = `${Number(m[1])}${m[2].toUpperCase()}`;
    } else if (/^\d+$/.test(mem)) {
      errors.mem = "Add a unit, like 16G or 500M.";
    } else {
      errors.mem = "Like 16G or 500M.";
    }
  }

  const gpus = form.gpus.trim();
  if (gpus !== "") {
    if (!/^\d+$/.test(gpus)) errors.gpus = "A whole number.";
    else if (Number(gpus) > 0) spec.gpus = Number(gpus);
  }

  for (const key of ["account", "qos", "constraint"] as const) {
    const v = form[key].trim();
    if (v !== "") spec[key] = v;
    else if (ctx.requires.includes(key)) {
      errors[key] =
        key === "account"
          ? "This cluster needs an account."
          : key === "qos"
            ? "This cluster needs a QOS."
            : "This cluster needs a constraint.";
    }
  }

  return Object.keys(errors).length === 0 ? { spec, errors } : { spec: null, errors };
}

/** A partition's node size from sinfo (`%c` CPUs, `%m` MB, "+" = varies upward). */
export function nodeSizeWords(p: PartitionChoice): string {
  const parts: string[] = [];
  const cpus = p.cpus_per_node.trim();
  if (cpus !== "") parts.push(`${cpus} CPUs`);
  const mem = p.mem_per_node.trim();
  const m = mem.match(/^(\d+)(\+?)$/);
  if (m !== null) parts.push(`${Math.round(Number(m[1]) / 1024)}${m[2]} GB`);
  else if (mem !== "") parts.push(mem);
  return parts.length === 0 ? "" : `Nodes have ${parts.join(" · ")}`;
}

/** Tags a partition row wears — only facts the cluster reported or taught us. */
export function partitionTags(
  p: PartitionChoice,
  config: ClusterConfig | null,
): { text: string; tone: "neutral" | "accent" | "warn" }[] {
  const tags: { text: string; tone: "neutral" | "accent" | "warn" }[] = [];
  if (p.default) tags.push({ text: "default", tone: "accent" });
  if (!p.up) tags.push({ text: "down", tone: "warn" });
  if (p.max_time_secs !== null) tags.push({ text: `up to ${limitWords(p.max_time_secs)}`, tone: "neutral" });
  else if (p.max_time.trim() !== "" && /^(UNLIMITED|INFINITE)$/i.test(p.max_time.trim())) {
    tags.push({ text: "no time limit", tone: "neutral" });
  }
  if (p.preemptible) tags.push({ text: "can be preempted", tone: "warn" });
  if (p.gpus) tags.push({ text: "GPUs", tone: "neutral" });
  if (isInteractiveOnly(config, p.name)) {
    tags.push({ text: "interactive only · stops when you disconnect", tone: "warn" });
  }
  return tags;
}

// --- the file peek ---------------------------------------------------------------

/** Folders first, then names (case-insensitive, numbers in order). */
export function sortEntries<T extends { name: string; dir: boolean }>(entries: readonly T[]): T[] {
  return [...entries].sort(
    (a, b) =>
      Number(b.dir) - Number(a.dir) ||
      a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" }),
  );
}

/** `dir` + `name` with one slash between ("/" stays the root). */
export function childPath(dir: string, name: string): string {
  return dir === "/" ? `/${name}` : `${dir.replace(/\/+$/, "")}/${name}`;
}

/** The folder above an absolute path; null at the root (or for a relative path). */
export function parentPath(path: string): string | null {
  const p = path.replace(/\/+$/, "");
  if (p === "" || !p.startsWith("/")) return null;
  const i = p.lastIndexOf("/");
  return i <= 0 ? "/" : p.slice(0, i);
}

// --- the job window -------------------------------------------------------------

/** Below this the job window offers to continue on a new node. */
export const CONTINUE_OFFER_SECS = HOUR;
/** Below this a dismissed offer comes back once. */
export const CONTINUE_LAST_CALL_SECS = 10 * MINUTE;

/** "Stops in 58 min." for the continue banner. */
export function stopsInWords(remainingSecs: number): string {
  if (remainingSecs < MINUTE) return "Stops in under a minute.";
  return `Stops in ${shortDuration(remainingSecs)}.`;
}

/** The ended overlay's reason in words; the shell's reason is Slurm's state or "stopped". */
export function endedReasonWords(reason: string | null | undefined): string {
  return endedWords(reason, false);
}
