import { describe, expect, it } from "vitest";

import type {
  ClusterConfig,
  ClusterFacts,
  ClusterJob,
  ClusterOverview,
  ClusterWorkspaceView,
  PartitionChoice,
} from "../net/native";
import {
  accountChoices,
  childPath,
  agoWords,
  buildSpec,
  clockWords,
  composeWalltime,
  defaultForm,
  endedLine,
  endedScreenWords,
  endedWords,
  formFromSpec,
  hostSummary,
  isInteractiveOnly,
  jobResources,
  jobStatusLine,
  limitWords,
  liveJobs,
  memWords,
  nodeSizeWords,
  filterFolders,
  isTypedPath,
  openInHint,
  splitTypedPath,
  openPlan,
  otherJobsWords,
  parentPath,
  parseSlurmTime,
  shortHost,
  tildePath,
  partitionTags,
  reasonWords,
  refusalField,
  shortDuration,
  splitWalltime,
  stopsInWords,
  timeLeftWords,
  walltimeSecs,
  workspaceActivity,
  type StartForm,
} from "./cluster";

const MIN = 60_000;
const HOUR = 60 * MIN;
const DAY = 24 * HOUR;
const NOW = new Date(2026, 8, 30, 10, 0, 0).getTime();

function ws(over: Partial<ClusterWorkspaceView> = {}): ClusterWorkspaceView {
  return {
    id: "w-0000aaaa",
    name: "proj",
    path: "/scratch/u/proj",
    state: "closed",
    ...over,
  };
}

function job(over: Partial<ClusterJob> = {}): ClusterJob {
  return {
    id: "j-0000aaaa",
    name: "Long",
    state: "running",
    attached: false,
    stopped_by_user: false,
    open: [],
    spec: { time: "7-00:00:00" },
    startup: "",
    submitted_ms: NOW - HOUR,
    ...over,
  };
}

function partition(over: Partial<PartitionChoice> = {}): PartitionChoice {
  return {
    name: "batch",
    default: false,
    max_time: "2-00:00:00",
    max_time_secs: 2 * 86400,
    cpus_per_node: "32",
    mem_per_node: "128000",
    gpus: false,
    preemptible: false,
    up: true,
    ...over,
  };
}

function facts(over: Partial<ClusterFacts> = {}): ClusterFacts {
  return {
    scheduler: "slurm",
    version: "23.11",
    partitions: [partition({ name: "batch", default: true }), partition({ name: "long", max_time_secs: 7 * 86400 })],
    accounts: [],
    default_account: null,
    fetched_ms: NOW,
    ...over,
  };
}

function config(over: Partial<ClusterConfig> = {}): ClusterConfig {
  return {
    version: 2,
    workspaces: [],
    setups: [],
    agent_rules: { text: "" },
    learned: {},
    ...over,
  };
}

function overview(jobs: ClusterJob[], workspaces: ClusterWorkspaceView[] = []): ClusterOverview {
  return {
    scheduler: "slurm",
    login_node: "login1",
    now_ms: NOW,
    jobs,
    workspaces,
    other_jobs: { running: 0, waiting: 0 },
    degraded: false,
    queue_at_ms: NOW,
    config: config(),
    startup: { cluster: "", workspaces: {} },
  };
}

function form(over: Partial<StartForm> = {}): StartForm {
  return {
    partition: "batch",
    days: 0,
    hours: 2,
    mins: 0,
    cpus: "",
    mem: "",
    gpus: "",
    account: "",
    qos: "",
    constraint: "",
    ...over,
  };
}

describe("durations", () => {
  it("shortens time left to the two largest units", () => {
    expect(shortDuration(5 * 86400 + 22 * 3600 + 59)).toBe("5d 22h");
    expect(shortDuration(3 * 3600 + 12 * 60)).toBe("3h 12m");
    expect(shortDuration(3 * 3600)).toBe("3h");
    expect(shortDuration(58 * 60 + 30)).toBe("58 min");
    expect(shortDuration(30)).toBe("under a minute");
    expect(shortDuration(-5)).toBe("under a minute");
  });

  it("says time left, and when it ran out", () => {
    expect(timeLeftWords(NOW + 2 * HOUR, NOW)).toBe("2h left");
    expect(timeLeftWords(NOW - MIN, NOW)).toBe("time's up");
  });

  it("words a partition's limit", () => {
    expect(limitWords(7 * 86400)).toBe("7 days");
    expect(limitWords(86400)).toBe("1 day");
    expect(limitWords(2 * 86400 + 12 * 3600)).toBe("2 days 12 hours");
    expect(limitWords(12 * 3600)).toBe("12 hours");
    expect(limitWords(30 * 60)).toBe("30 minutes");
    expect(limitWords(90 * 60)).toBe("1 hour 30 minutes");
  });

  it("says how long ago", () => {
    expect(agoWords(NOW - 20_000, NOW)).toBe("just now");
    expect(agoWords(NOW - 5 * MIN, NOW)).toBe("5 min ago");
    expect(agoWords(NOW - 3 * HOUR, NOW)).toBe("3 h ago");
    expect(agoWords(NOW - 30 * HOUR, NOW)).toBe("yesterday");
    expect(agoWords(NOW - 4 * DAY, NOW)).toBe("4 days ago");
  });

  it("puts a start estimate on a clock", () => {
    const later = new Date(2026, 8, 30, 14, 20).getTime();
    const tomorrow = new Date(2026, 9, 1, 9, 0).getTime();
    expect(clockWords(later, NOW, "en-GB")).toBe("14:20");
    expect(clockWords(tomorrow, NOW, "en-GB")).toBe("tomorrow 09:00");
    expect(clockWords(new Date(2026, 9, 3, 9, 0).getTime(), NOW, "en-GB")).toMatch(/^\S+ 09:00$/);
  });

  it("words the continue banner", () => {
    expect(stopsInWords(58 * 60)).toBe("This job ends in 58 min.");
    expect(stopsInWords(20)).toBe("This job ends in under a minute.");
  });
});

describe("Slurm walltime", () => {
  it("reads Slurm's input forms", () => {
    expect(parseSlurmTime("90")).toBe(90 * 60);
    expect(parseSlurmTime("10:30")).toBe(10 * 60 + 30);
    expect(parseSlurmTime("04:00:00")).toBe(4 * 3600);
    expect(parseSlurmTime("2-00:00:00")).toBe(2 * 86400);
    expect(parseSlurmTime("1-12")).toBe(86400 + 12 * 3600);
    expect(parseSlurmTime("1-12:30")).toBe(86400 + 12 * 3600 + 30 * 60);
    expect(parseSlurmTime("UNLIMITED")).toBeNull();
    expect(parseSlurmTime("")).toBeNull();
  });

  it("composes D-HH:MM:SS from the boxes", () => {
    expect(composeWalltime(walltimeSecs({ days: 0, hours: 2, mins: 0 }))).toBe("02:00:00");
    expect(composeWalltime(walltimeSecs({ days: 2, hours: 0, mins: 5 }))).toBe("2-00:05:00");
    expect(composeWalltime(walltimeSecs({ days: null, hours: null, mins: 45 }))).toBe("00:45:00");
    expect(composeWalltime(walltimeSecs({ days: 0, hours: 0, mins: 0 }))).toBe("");
    // Odd entries floor to whole units and never go negative.
    expect(walltimeSecs({ days: -1, hours: 1.9, mins: null })).toBe(3600);
  });

  it("round-trips through the boxes", () => {
    const secs = parseSlurmTime("1-04:30:00");
    expect(secs).not.toBeNull();
    const boxes = splitWalltime(secs ?? 0);
    expect(boxes).toEqual({ days: 1, hours: 4, mins: 30 });
    expect(composeWalltime(walltimeSecs(boxes))).toBe("1-04:30:00");
  });

  it("never shortens a limit with seconds in it", () => {
    expect(splitWalltime(61)).toEqual({ days: 0, hours: 0, mins: 2 });
  });
});

describe("resources", () => {
  it("words memory", () => {
    expect(memWords("16G")).toBe("16 GB");
    expect(memWords("4096M")).toBe("4 GB");
    expect(memWords("4000M")).toBe("4000 MB");
    expect(memWords("2048")).toBe("2 GB");
    expect(memWords("1T")).toBe("1 TB");
    expect(memWords("")).toBe("");
    expect(memWords(undefined)).toBe("");
    expect(memWords("weird")).toBe("weird");
  });

  it("joins what the job carries", () => {
    expect(jobResources({ cpus: "8", mem: "32G", gpus: 1 })).toBe("8 CPUs · 32 GB · 1 GPU");
    expect(jobResources({ cpus: "1", gpus: 2 })).toBe("1 CPU · 2 GPUs");
    expect(jobResources({})).toBe("");
  });
});

describe("jobs in words", () => {
  it("names why a job ended", () => {
    expect(endedWords("TIMEOUT", false)).toBe("it hit its time limit");
    expect(endedWords("CANCELLED by 1234", false)).toBe("it was cancelled");
    expect(endedWords("CANCELLED+", true)).toBe("you stopped it");
    expect(endedWords("stopped", false)).toBe("you stopped it");
    expect(endedWords("NODE_FAIL", false)).toBe("its node failed");
    expect(endedWords("OUT_OF_MEMORY", false)).toBe("it ran out of memory");
    expect(endedWords("ENDED", false)).toBe("");
    expect(endedWords(undefined, false)).toBe("");
  });

  it("says why a job waits only when it isn't plain priority", () => {
    expect(reasonWords("Priority")).toBeNull();
    expect(reasonWords("None")).toBeNull();
    expect(reasonWords(undefined)).toBeNull();
    expect(reasonWords("Resources")).toBe("waiting for the nodes it needs to free up");
    expect(reasonWords("QOSMaxJobsPerUserLimit")).toBe(
      "you're at a limit on this cluster (QOSMaxJobsPerUserLimit)",
    );
    expect(reasonWords("ReqNodeNotAvail, Reserved for maintenance")).toBe(
      "a node it needs isn't available",
    );
    expect(reasonWords("SomethingNew")).toBe("Slurm says: SomethingNew");
  });

  it("gives each job card one status line", () => {
    const running = job({
      node: "n042",
      cpus: "8",
      mem: "32G",
      ends_at_ms: NOW + 5 * DAY + 22 * HOUR + 10 * MIN,
    });
    expect(jobStatusLine(running, NOW)).toBe("On n042 · 8 CPUs · 32 GB · ends in 5d 22h");
    expect(jobStatusLine(job({ state: "starting", node: "n7" }), NOW)).toBe("Starting on n7…");
    expect(jobStatusLine(job({ state: "waiting", submitted_ms: NOW - 20_000 }), NOW)).toBe(
      "Waiting for a node",
    );
    expect(jobStatusLine(job({ state: "waiting", submitted_ms: NOW - 4 * MIN - 5_000 }), NOW)).toBe(
      "Waiting for a node · 4 min so far",
    );
    const est = new Date(2026, 8, 30, 14, 20).getTime();
    expect(
      jobStatusLine(job({ state: "waiting", start_estimate_ms: est, reason: "Resources" }), NOW, "en-GB"),
    ).toBe(
      "Waiting for a node · 1h so far · Slurm estimates 14:20 · waiting for the nodes it needs to free up",
    );
    expect(jobStatusLine(job({ state: "waiting", attached: true, submitted_ms: NOW }), NOW)).toBe(
      "Waiting for a node · stops if this app disconnects",
    );
  });

  it("words an ended job as one line", () => {
    expect(endedLine(job({ state: "ended", ended: "TIMEOUT", ended_at_ms: NOW - 2 * HOUR }), NOW)).toBe(
      "Long ended 2 h ago — it hit its time limit. Chats are saved.",
    );
    expect(endedLine(job({ state: "ended", stopped_by_user: true }), NOW)).toBe(
      "Long ended — you stopped it. Chats are saved.",
    );
    expect(endedLine(job({ state: "ended" }), NOW)).toBe("Long ended. Chats are saved.");
  });

  it("words the window's ended screen", () => {
    expect(endedScreenWords("TIMEOUT")).toBe(
      "This job ended — it hit its time limit. Your chats are saved.",
    );
    expect(endedScreenWords("closed")).toBe("This workspace was closed. Its chats are saved.");
    expect(endedScreenWords("moving")).toBe(
      "Moving to the new job — this window reopens there when it's ready. Your chats come with you.",
    );
    expect(endedScreenWords("workspace-failed")).toBe(
      "This workspace stopped unexpectedly. Its chats are saved — open it again from the cluster page.",
    );
    expect(endedScreenWords(null)).toBe("This job ended. Your chats are saved.");
  });

  it("says what a workspace is doing", () => {
    expect(workspaceActivity(ws({ state: "open", working: 2 }), NOW)).toBe("2 chats working");
    expect(workspaceActivity(ws({ state: "open", working: 1 }), NOW)).toBe("1 chat working");
    expect(workspaceActivity(ws({ state: "open", working: 0 }), NOW)).toBe("idle");
    expect(workspaceActivity(ws({ state: "open" }), NOW)).toBe("");
    expect(workspaceActivity(ws({ state: "open", failed: "boom" }), NOW)).toBe("stopped unexpectedly");
    expect(workspaceActivity(ws({ state: "closed", job: "j-0000aaaa", failed: "" }), NOW)).toBe(
      "stopped unexpectedly",
    );
    expect(workspaceActivity(ws({ state: "queued" }), NOW)).toBe("opens when it starts");
    expect(workspaceActivity(ws({ state: "queued", opening: true }), NOW)).toBe("opening…");
    expect(workspaceActivity(ws({ state: "open", working: 1, closing: true }), NOW)).toBe(
      "closing — saving its chats…",
    );
    expect(workspaceActivity(ws({ last_open_ms: NOW - 2 * DAY }), NOW)).toBe(
      "last open 2 days ago · chats saved",
    );
    expect(workspaceActivity(ws(), NOW)).toBe("not opened yet");
  });

  it("lets you pick where Open goes whenever a job is alive", () => {
    expect(openPlan([])).toEqual({ kind: "sheet" });
    expect(openPlan([job({ state: "ended" })])).toEqual({ kind: "sheet" });
    const waiting = job({ id: "j-0000000w", state: "waiting" });
    expect(openPlan([waiting])).toEqual({ kind: "choose", running: [], pending: [waiting] });
    // One running job is still a choice: it shares that job's node and time.
    const one = job({ id: "j-0000bbbb" });
    expect(openPlan([one, waiting])).toEqual({ kind: "choose", running: [one], pending: [waiting] });
    const two = job({ id: "j-0000cccc", name: "GPU" });
    expect(openPlan([one, two])).toEqual({ kind: "choose", running: [one, two], pending: [] });
  });

  it("filters a folder listing as you type, best matches first", () => {
    const f = (...names: string[]) => names.map((name) => ({ name }));
    const all = f("ajd", "Jdoe", "j_dodo", "notes", "xjdox");
    expect(filterFolders(all, "").map((x) => x.name)).toEqual(all.map((x) => x.name));
    expect(filterFolders(all, "JDO").map((x) => x.name)).toEqual(["Jdoe", "xjdox", "j_dodo"]);
    expect(filterFolders(all, " jd ").map((x) => x.name)).toEqual(["Jdoe", "ajd", "xjdox", "j_dodo"]);
    expect(filterFolders(all, "zzz")).toEqual([]);
  });

  it("tells a typed path from a filter, and splits it for completion", () => {
    expect(isTypedPath("/scratch/u")).toBe(true);
    expect(isTypedPath("~/proj")).toBe(true);
    expect(isTypedPath("$SCRATCH/x")).toBe(true);
    expect(isTypedPath("proj")).toBe(false);
    expect(splitTypedPath("~/proj/an")).toEqual({ dir: "~/proj/", tail: "an" });
    expect(splitTypedPath("/scratch/users/")).toEqual({ dir: "/scratch/users/", tail: "" });
    expect(splitTypedPath("~")).toEqual({ dir: "~", tail: "" });
    expect(splitTypedPath("$SCRATCH")).toEqual({ dir: "$SCRATCH", tail: "" });
  });

  it("says where a running job would open a workspace", () => {
    const j = job({ node: "node042.cluster.example", ends_at_ms: NOW + 27 * MIN });
    expect(openInHint(j, ["crc"], NOW)).toBe("node042 · ends in 27 min · with crc");
    expect(openInHint(job({ node: "" }), [], NOW)).toBe("");
  });

  it("shows live jobs newest first", () => {
    const old = job({ id: "j-1", submitted_ms: 1 });
    const fresh = job({ id: "j-2", submitted_ms: 2, state: "waiting" });
    const gone = job({ id: "j-3", state: "ended" });
    expect(liveJobs([old, gone, fresh]).map((j) => j.id)).toEqual(["j-2", "j-1"]);
  });

  it("summarizes a cluster for its host row", () => {
    expect(hostSummary(overview([]), NOW)).toBe("no jobs running");
    expect(hostSummary(overview([job({ state: "ended" })]), NOW)).toBe("no jobs running");
    expect(
      hostSummary(overview([job({ ends_at_ms: NOW + 5 * DAY + 22 * HOUR + 5 * MIN })]), NOW),
    ).toBe("1 job running · ends in 5d 22h");
    expect(
      hostSummary(
        overview([
          job({ ends_at_ms: NOW + 5 * DAY }),
          job({ id: "j-2", ends_at_ms: NOW + 3 * HOUR + 40 * MIN }),
        ]),
        NOW,
      ),
    ).toBe("2 jobs running · next ends in 3h 40m");
    expect(hostSummary(overview([job({ state: "waiting" })]), NOW)).toBe("1 job waiting for a node");
    expect(hostSummary(overview([job(), job({ id: "j-2", state: "waiting" })]), NOW)).toBe(
      "1 job running · 1 waiting",
    );
    expect(hostSummary(overview([job({ state: "starting" })]), NOW)).toBe("1 job starting");
  });

  it("shortens paths under the cluster's home and login node names", () => {
    expect(tildePath("/home/u/projects/crc", "/home/u")).toBe("~/projects/crc");
    expect(tildePath("/home/u", "/home/u/")).toBe("~");
    expect(tildePath("/home/user2/x", "/home/u")).toBe("/home/user2/x");
    expect(tildePath("/scratch/u/x", "/home/u")).toBe("/scratch/u/x");
    expect(tildePath("/scratch/u/x", undefined)).toBe("/scratch/u/x");
    expect(shortHost("login2.cluster.example.edu")).toBe("login2");
    expect(shortHost("login2")).toBe("login2");
  });

  it("counts the user's other Slurm jobs", () => {
    expect(otherJobsWords({ running: 2, waiting: 9 })).toBe("Your other Slurm jobs: 2 running · 9 waiting");
    expect(otherJobsWords({ running: 0, waiting: 0 })).toBe("");
  });
});

describe("the start sheet", () => {
  it("starts from the default partition and two hours", () => {
    const f = defaultForm(facts());
    expect(f.partition).toBe("batch");
    expect(walltimeSecs(f)).toBe(2 * 3600);
    expect(defaultForm(null).partition).toBe("");
  });

  it("restores a saved spec", () => {
    const f = formFromSpec(
      { time: "1-00:00:00", partition: "long", cpus: 8, mem: "32G", gpus: 0, account: "lab" },
      facts(),
    );
    expect(f).toMatchObject({ partition: "long", days: 1, hours: 0, mins: 0, cpus: "8", mem: "32G", gpus: "", account: "lab" });
  });

  it("composes a spec, omitting blanks", () => {
    const { spec, errors } = buildSpec(form({ cpus: "4", mem: "16g", gpus: "1" }), {
      partition: partition({ name: "batch" }),
      requires: [],
    });
    expect(errors).toEqual({});
    expect(spec).toEqual({ time: "02:00:00", partition: "batch", cpus: 4, mem: "16G", gpus: 1 });
  });

  it("requires a time and pre-flights it against the partition", () => {
    expect(buildSpec(form({ hours: 0 }), { partition: null, requires: [] }).errors.time).toMatch(/time limit/);
    const over = buildSpec(form({ days: 3 }), { partition: partition({ name: "batch" }), requires: [] });
    expect(over.spec).toBeNull();
    expect(over.errors.time).toBe("batch allows up to 2 days.");
    // No published limit: Slurm stays the judge.
    expect(
      buildSpec(form({ days: 30 }), { partition: partition({ max_time_secs: null }), requires: [] }).errors.time,
    ).toBeUndefined();
  });

  it("rejects bad resource entries with a line each", () => {
    const { spec, errors } = buildSpec(form({ cpus: "0", mem: "16", gpus: "x" }), {
      partition: null,
      requires: [],
    });
    expect(spec).toBeNull();
    expect(errors.cpus).toBeDefined();
    expect(errors.mem).toBe("Add a unit, like 16G or 500M.");
    expect(errors.gpus).toBeDefined();
    expect(buildSpec(form({ mem: "lots" }), { partition: null, requires: [] }).errors.mem).toBe(
      "Like 16G or 500M.",
    );
  });

  it("treats zero GPUs as none", () => {
    expect(buildSpec(form({ gpus: "0" }), { partition: null, requires: [] }).spec).toEqual({
      time: "02:00:00",
      partition: "batch",
    });
  });

  it("makes learned fields required", () => {
    const { spec, errors } = buildSpec(form(), { partition: null, requires: ["account", "qos"] });
    expect(spec).toBeNull();
    expect(errors.account).toBe("This cluster needs an account.");
    expect(errors.qos).toBe("This cluster needs a QOS.");
    expect(errors.constraint).toBeUndefined();
    const ok = buildSpec(form({ account: " lab ", qos: "normal" }), { partition: null, requires: ["account", "qos"] });
    expect(ok.spec).toMatchObject({ account: "lab", qos: "normal" });
  });

  it("limits accounts to the partition's own list", () => {
    const f = facts({ accounts: ["lab", "other"], default_account: "lab" });
    expect(accountChoices(f, partition({ accounts: ["other"] }))).toEqual(["other"]);
    expect(accountChoices(f, partition({ accounts: [] }))).toEqual(["lab", "other"]);
    expect(accountChoices(null, null)).toEqual([]);
  });

  it("tags partitions with reported facts only", () => {
    const cfg = config({ learned: { interactive_only: ["dev"] } });
    const texts = (p: PartitionChoice) => partitionTags(p, cfg).map((t) => t.text);
    expect(texts(partition({ name: "batch", default: true }))).toEqual(["default", "up to 2 days"]);
    expect(texts(partition({ name: "dev", max_time_secs: 3600, preemptible: true, gpus: true }))).toEqual([
      "up to 1 hour",
      "can be preempted",
      "GPUs",
      "interactive only · stops when you disconnect",
    ]);
    expect(texts(partition({ up: false, max_time: "UNLIMITED", max_time_secs: null }))).toEqual([
      "down",
      "no time limit",
    ]);
    expect(isInteractiveOnly(cfg, "dev")).toBe(true);
    expect(isInteractiveOnly(cfg, "")).toBe(false);
  });

  it("words a partition's node size", () => {
    expect(nodeSizeWords(partition({ cpus_per_node: "32", mem_per_node: "128000" }))).toBe(
      "Nodes have 32 CPUs · 125 GB",
    );
    expect(nodeSizeWords(partition({ cpus_per_node: "", mem_per_node: "256000+" }))).toBe("Nodes have 250+ GB");
    expect(nodeSizeWords(partition({ cpus_per_node: "", mem_per_node: "" }))).toBe("");
  });

  it("maps refusals to the field to reveal", () => {
    expect(refusalField("account_required")).toBe("account");
    expect(refusalField("qos_required")).toBe("qos");
    expect(refusalField("constraint_required")).toBe("constraint");
    expect(refusalField("batch_not_allowed")).toBeNull();
    expect(refusalField("other")).toBeNull();
  });
});

describe("the folder picker", () => {
  it("walks paths", () => {
    expect(childPath("/scratch/u", "data")).toBe("/scratch/u/data");
    expect(childPath("/scratch/u/", "data")).toBe("/scratch/u/data");
    expect(childPath("/", "home")).toBe("/home");
    expect(parentPath("/scratch/u/data")).toBe("/scratch/u");
    expect(parentPath("/scratch")).toBe("/");
    expect(parentPath("/")).toBeNull();
    expect(parentPath("~/x")).toBeNull();
  });
});
