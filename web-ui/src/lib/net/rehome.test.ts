import { describe, expect, it } from "vitest";
import type { JobContext } from "./api";
import { rehomeUrl } from "./rehome";

// Characterization oracles: the two re-home builders exactly as App.svelte
// wrote them before `rehome.ts` existed (handleHostStatus's connected branch
// and the onLocalDaemonUpdated listener), with App's closure reads lifted
// into arguments. `rehomeUrl` must reproduce both byte for byte.
function legacyHostStatusUrl(a: {
  token: string | null;
  win: string;
  activeWsId: string | null;
  carryHub: boolean;
  hostAlias: string;
  appearance: unknown;
  jobCtx: JobContext | null;
  port: number | null;
  locationPort: string;
}): string {
  const params = new URLSearchParams();
  if (a.token !== null) params.set("token", a.token);
  params.set("win", a.win);
  if (a.activeWsId !== null) params.set("ws", a.activeWsId);
  if (a.carryHub) params.set("hub", "1");
  params.set("host", a.hostAlias);
  params.set("appearance", JSON.stringify(a.appearance));
  if (a.jobCtx !== null) {
    params.set("job", a.jobCtx.jobId);
    if (a.jobCtx.node !== null) params.set("node", a.jobCtx.node);
    if (a.jobCtx.cws !== null) params.set("cws", a.jobCtx.cws);
  }
  return `http://127.0.0.1:${a.port ?? a.locationPort}/#${params.toString()}`;
}

function legacyLocalDaemonUrl(a: {
  token: string;
  win: string;
  activeWsId: string | null;
  carryHub: boolean;
  appearance: unknown;
  port: number;
}): string {
  const params = new URLSearchParams();
  params.set("token", a.token);
  params.set("win", a.win);
  if (a.activeWsId !== null) params.set("ws", a.activeWsId);
  if (a.carryHub) params.set("hub", "1");
  params.set("appearance", JSON.stringify(a.appearance));
  return `http://127.0.0.1:${a.port}/#${params.toString()}`;
}

const appearance = { theme: "dark", accent: "#5b8", font: "x y&z" };

describe("rehomeUrl: the remote tunnel heal (host-status connected)", () => {
  const jobs: (JobContext | null)[] = [
    null,
    { jobId: "123", node: null, cws: null },
    { jobId: "123", node: "n07", cws: null },
    { jobId: "123", node: "n07", cws: "w-abc" },
    { jobId: "9", node: null, cws: "w-x" },
  ];
  for (const token of [null, "tok/+=&"])
    for (const ws of [null, "w-1"])
      for (const hub of [false, true])
        for (const port of [null, 41234])
          for (const job of jobs) {
            it(`token=${token} ws=${ws} hub=${hub} port=${port} job=${JSON.stringify(job)}`, () => {
              const expected = legacyHostStatusUrl({
                token,
                win: "win-1",
                activeWsId: ws,
                carryHub: hub,
                hostAlias: "cluster",
                appearance,
                jobCtx: job,
                port,
                locationPort: "5555",
              });
              expect(
                rehomeUrl({
                  port: port ?? "5555",
                  token,
                  win: "win-1",
                  ws,
                  hub,
                  host: "cluster",
                  appearance,
                  job,
                }),
              ).toBe(expected);
            });
          }
});

describe("rehomeUrl: the local daemon's self-update", () => {
  for (const ws of [null, "w-1"])
    for (const hub of [false, true]) {
      it(`ws=${ws} hub=${hub}`, () => {
        const expected = legacyLocalDaemonUrl({
          token: "t0k",
          win: "win-2",
          activeWsId: ws,
          carryHub: hub,
          appearance,
          port: 40001,
        });
        expect(
          rehomeUrl({
            port: 40001,
            token: "t0k",
            win: "win-2",
            ws,
            hub,
            host: null,
            appearance,
            job: null,
          }),
        ).toBe(expected);
      });
    }
});

describe("rehomeUrl: pinned literals", () => {
  it("orders the fragment token, win, ws, hub, host, appearance, job, node, cws", () => {
    expect(
      rehomeUrl({
        port: 7,
        token: "t",
        win: "w",
        ws: "s",
        hub: true,
        host: "h",
        appearance: { a: 1 },
        job: { jobId: "j", node: "n", cws: "c" },
      }),
    ).toBe(
      "http://127.0.0.1:7/#token=t&win=w&ws=s&hub=1&host=h&appearance=%7B%22a%22%3A1%7D&job=j&node=n&cws=c",
    );
  });

  it("a local re-home names no host and no job", () => {
    expect(
      rehomeUrl({
        port: 8,
        token: "t",
        win: "w",
        ws: null,
        hub: false,
        host: null,
        appearance: null,
        job: null,
      }),
    ).toBe("http://127.0.0.1:8/#token=t&win=w&appearance=null");
  });
});
