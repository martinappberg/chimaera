/**
 * Pure helpers for the cluster surfaces (the host row, the cluster page, the
 * start sheet, the folder picker, the workspace window's notices): job and
 * workspace state → plain words, the start sheet's form → a Slurm launch
 * spec, and time formatting. The words follow docs/hpc-portal-plan.md §4:
 * cluster, job, workspace, chats — never server, session, allocation.
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
  ClusterJob,
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

/** "8 CPUs · 32 GB · 1 GPU" — whatever the job carries. */
export function jobResources(j: Pick<ClusterJob, "cpus" | "mem" | "gpus">): string {
  const parts: string[] = [];
  const cpus = j.cpus?.trim() ?? "";
  if (cpus !== "") parts.push(cpus === "1" ? "1 CPU" : `${cpus} CPUs`);
  const mem = memWords(j.mem);
  if (mem !== "") parts.push(mem);
  if (j.gpus !== undefined && j.gpus > 0) parts.push(j.gpus === 1 ? "1 GPU" : `${j.gpus} GPUs`);
  return parts.join(" · ");
}

// --- jobs → words ----------------------------------------------------------------

/** The first word of a Slurm state (`CANCELLED by 1234`, `CANCELLED+`). */
function stateWord(state: string | null | undefined): string {
  return (state ?? "").trim().toUpperCase().split(/[\s+]/)[0] ?? "";
}

/**
 * How a job ended, as the end of a sentence: "it hit its time limit". The
 * shell reports "stopped" for a stop made from the app and "closed" when a
 * window's workspace was closed (its job still runs).
 */
export function endedWords(ended: string | undefined | null, stoppedByUser: boolean): string {
  if (stoppedByUser) return "you stopped it";
  switch (stateWord(ended)) {
    case "STOPPED":
      return "you stopped it";
    case "TIMEOUT":
      return "it hit its time limit";
    case "CANCELLED":
      return "it was cancelled";
    case "FAILED":
      return "it failed";
    case "PREEMPTED":
      return "it was preempted";
    case "NODE_FAIL":
      return "its node failed";
    case "OUT_OF_MEMORY":
      return "it ran out of memory";
    case "COMPLETED":
      return "it finished";
    default:
      return "";
  }
}

/**
 * Why a job waits, in plain words — `null` for ordinary priority (not worth
 * saying). Reasons without a plain reading stay Slurm's own word: there,
 * Slurm is the speaker.
 */
export function reasonWords(reason: string | null | undefined): string | null {
  const r = (reason ?? "").trim();
  if (r === "" || r === "Priority" || r === "None") return null;
  if (r === "Resources") return "waiting for the nodes it needs to free up";
  if (r === "Dependency") return "waiting for another job to finish";
  if (r === "BeginTime") return "set to start later";
  if (r === "PartitionDown" || r === "PartitionInactive") return "its partition is down";
  if (r.startsWith("ReqNodeNotAvail")) return "a node it needs isn't available";
  if (r === "JobHeldUser" || r === "JobHeldAdmin") return "on hold";
  if (r === "Reservation") return "waiting for a reservation";
  if (/Limit/.test(r)) return `you're at a limit on this cluster (${r})`;
  return `Slurm says: ${r}`;
}

/** "ends in 5d 22h", or "time's up". */
export function endsInWords(endsAtMs: number, nowMs: number): string {
  const secs = Math.floor((endsAtMs - nowMs) / 1000);
  if (secs <= 0) return "time's up";
  return `ends in ${shortDuration(secs)}`;
}

/** "/home/u/x" → "~/x" under the cluster's home folder; anything else as is. */
export function tildePath(path: string, home: string | undefined): string {
  const h = (home ?? "").replace(/\/+$/, "");
  if (h === "") return path;
  if (path === h) return "~";
  return path.startsWith(`${h}/`) ? `~${path.slice(h.length)}` : path;
}

/** "sh01.cluster.example.edu" → "sh01": a login node's short name. */
export function shortHost(host: string): string {
  return host.split(".")[0] || host;
}

/** A job card's one status line (plan §4.2). */
export function jobStatusLine(j: ClusterJob, nowMs: number, locale?: string): string {
  let line: string;
  switch (j.state) {
    case "running": {
      const parts: string[] = [j.node ? `On ${j.node}` : "Running"];
      const res = jobResources(j);
      if (res !== "") parts.push(res);
      if (j.ends_at_ms !== undefined) parts.push(endsInWords(j.ends_at_ms, nowMs));
      line = parts.join(" · ");
      break;
    }
    case "starting":
      line = j.node ? `Starting on ${j.node}…` : "Starting…";
      break;
    case "waiting": {
      line = "Waiting for a node";
      const waited = (nowMs - j.submitted_ms) / 1000;
      if (waited >= MINUTE) line += ` · ${shortDuration(waited)} so far`;
      if (j.start_estimate_ms !== undefined && j.start_estimate_ms !== null) {
        line += ` · Slurm estimates ${clockWords(j.start_estimate_ms, nowMs, locale)}`;
      }
      const why = reasonWords(j.reason);
      if (why !== null) line += ` · ${why}`;
      break;
    }
    case "ended":
      return endedLine(j, nowMs, locale);
  }
  if (j.attached) line += " · stops if this app disconnects";
  return line;
}

/** "Long ended 2 h ago — it hit its time limit. Chats are saved." */
export function endedLine(j: ClusterJob, nowMs: number, locale?: string): string {
  const when = j.ended_at_ms !== undefined ? ` ${agoWords(j.ended_at_ms, nowMs, locale)}` : "";
  const why = endedWords(j.ended, j.stopped_by_user);
  return `${j.name} ended${when}${why === "" ? "" : ` — ${why}`}. Chats are saved.`;
}

/** What a workspace row says about itself (plan §4.2). */
export function workspaceActivity(w: ClusterWorkspaceView, nowMs: number, locale?: string): string {
  if (w.failed !== undefined) return "stopped unexpectedly";
  switch (w.state) {
    case "open":
      if (w.closing) return "closing — saving its chats…";
      if (w.working === undefined) return "";
      if (w.working === 0) return "idle";
      return w.working === 1 ? "1 chat working" : `${w.working} chats working`;
    case "queued":
      return w.opening ? "opening…" : "opens when it starts";
    case "closed":
      return w.last_open_ms !== undefined
        ? `last open ${agoWords(w.last_open_ms, nowMs, locale)} · chats saved`
        : "not opened yet";
  }
}

/** Where Open sends a workspace that isn't open: always the user's pick —
 *  a running job (it shares that job's node and time), a waiting one (it
 *  opens when that one starts), or a new job. With no job alive, straight
 *  to the start sheet. */
export type OpenPlan =
  | { kind: "sheet" }
  | { kind: "choose"; running: ClusterJob[]; pending: ClusterJob[] };

export function openPlan(jobs: readonly ClusterJob[]): OpenPlan {
  const running = jobs.filter((j) => j.state === "running");
  const pending = jobs.filter((j) => j.state === "waiting" || j.state === "starting");
  if (running.length === 0 && pending.length === 0) return { kind: "sheet" };
  return { kind: "choose", running, pending };
}

/** The Open menu's line under a running job: where it runs, how long it
 *  has, and what's already open in it ("sh04 · ends in 27 min · with crc"). */
export function openInHint(j: ClusterJob, openNames: readonly string[], nowMs: number): string {
  const parts: string[] = [];
  if (j.node) parts.push(shortHost(j.node));
  if (j.ends_at_ms !== undefined) parts.push(endsInWords(j.ends_at_ms, nowMs));
  if (openNames.length > 0) parts.push(`with ${openNames.join(", ")}`);
  return parts.join(" · ");
}

/** Jobs shown as cards: everything not ended, newest first. */
export function liveJobs(jobs: readonly ClusterJob[]): ClusterJob[] {
  return jobs.filter((j) => j.state !== "ended").sort((a, b) => b.submitted_ms - a.submitted_ms);
}

/** "Your other Slurm jobs: 37 running · 4 waiting" (count only); "" when none. */
export function otherJobsWords(other: ClusterOverview["other_jobs"]): string {
  if (other.running === 0 && other.waiting === 0) return "";
  return `Your other Slurm jobs: ${other.running} running · ${other.waiting} waiting`;
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

// --- the folder picker -------------------------------------------------------------

/** `dir` + `name` with one slash between ("/" stays the root). */
export function childPath(dir: string, name: string): string {
  return dir === "/" ? `/${name}` : `${dir.replace(/\/+$/, "")}/${name}`;
}

/** The folder picker's box holds a path (`/…`, `~…`, `$VAR…`), not a filter. */
export function isTypedPath(text: string): boolean {
  return /^[/~$]/.test(text.trim());
}

/** A typed path as the folder to list and what to match in it:
 *  "~/proj/an" → { dir: "~/proj/", tail: "an" }; "~" → { dir: "~", tail: "" }. */
export function splitTypedPath(text: string): { dir: string; tail: string } {
  const t = text.trim();
  const slash = t.lastIndexOf("/");
  if (slash < 0) return { dir: t, tail: "" };
  return { dir: t.slice(0, slash + 1), tail: t.slice(slash + 1) };
}

/** Folders matching what was typed, case-insensitive: names that start with
 *  it first, then names containing it, then names with its letters in order
 *  ("jdo" finds "j_dodo"); each group keeps the listing's order. */
export function filterFolders<T extends { name: string }>(folders: readonly T[], query: string): T[] {
  const q = query.trim().toLowerCase();
  if (q === "") return [...folders];
  const prefix: T[] = [];
  const inside: T[] = [];
  const loose: T[] = [];
  for (const f of folders) {
    const n = f.name.toLowerCase();
    if (n.startsWith(q)) prefix.push(f);
    else if (n.includes(q)) inside.push(f);
    else if (inOrder(q, n)) loose.push(f);
  }
  return [...prefix, ...inside, ...loose];
}

function inOrder(needle: string, hay: string): boolean {
  let i = 0;
  for (const c of hay) {
    if (c === needle[i] && ++i === needle.length) return true;
  }
  return false;
}

/** The folder above an absolute path; null at the root (or for a relative path). */
export function parentPath(path: string): string | null {
  const p = path.replace(/\/+$/, "");
  if (p === "" || !p.startsWith("/")) return null;
  const i = p.lastIndexOf("/");
  return i <= 0 ? "/" : p.slice(0, i);
}

// --- the job window -------------------------------------------------------------

/** Below this the workspace window offers to continue in a new job. */
export const CONTINUE_OFFER_SECS = HOUR;
/** Below this a dismissed offer comes back once. */
export const CONTINUE_LAST_CALL_SECS = 10 * MINUTE;

/** "This job ends in 58 min." for the window's banner. */
export function stopsInWords(remainingSecs: number): string {
  if (remainingSecs < MINUTE) return "This job ends in under a minute.";
  return `This job ends in ${shortDuration(remainingSecs)}.`;
}

/**
 * The window's ended screen, from the shell's reason: Slurm's state,
 * "stopped" (stopped from the app), "closed" (this workspace was closed; its
 * job still runs), "workspace-failed" (its chimaera stopped on its own) or
 * "moving" (it is on its way to another job; the window follows).
 */
export function endedScreenWords(reason: string | null | undefined): string {
  switch (stateWord(reason)) {
    case "CLOSED":
      return "This workspace was closed. Its chats are saved.";
    case "WORKSPACE-FAILED":
      return "This workspace stopped unexpectedly. Its chats are saved — open it again from the cluster page.";
    case "MOVING":
      return "Moving to the new job — this window reopens there when it's ready. Your chats come with you.";
  }
  const why = endedWords(reason, false);
  return why === ""
    ? "This job ended. Your chats are saved."
    : `This job ended — ${why}. Your chats are saved.`;
}
