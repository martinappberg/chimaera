/**
 * The window's connection to its daemon, as App coordinates it: authenticated
 * HTTP health, the `/ws/events` up/down flag, the native remote window's SSH
 * reconnect (status/failure overlay, the 401 latch), build moves that force a
 * re-home or reload, and the shell's window-scope registry.
 *
 * App stays the composition owner. It constructs one `Connection` during its
 * own initialisation and calls each `track*` / `poll*` method at the place in
 * its script where those effects used to be declared, so the window's effect
 * order is unchanged; the `listen*` methods are called from App's `onMount`
 * in the same order as before and return the unlisten App already tears down.
 *
 * Ordering that matters:
 * - The constructor declares the 401 effect, then the two window-scope
 *   effects (identity, then debounced label).
 * - `trackHealth` declares the health poll before the kick effect: the kick
 *   reads the handle the poll effect assigns.
 */
import { fromStore, get } from "svelte/store";
import { untrack } from "svelte";
import {
  clearUnauthorized,
  getHostLabel,
  getJobContext,
  getToken,
  isHomeHub,
  pollHealth,
  reclaimHomeHub,
  unauthorized,
  type Health,
  type JobContext,
} from "./api";
import type { EventsSocket } from "./events";
import {
  clusterOpen,
  connectHost,
  isNativeShell,
  onAppUpdate,
  onHostStatus,
  onLocalDaemonUpdated,
  reportWindowScope,
  shellBuild,
  appUpdateStatus,
  type AppUpdateStatus,
  type HostStatusEvent,
} from "./native";
import { healthPollDelayMs, type PollHandle } from "./poll";
import {
  connectResultError,
  createReconnectListenerGate,
  hostStatusAction,
  selectRemoteReconnectSurface,
} from "./remoteReconnect";
import { rehomeUrl } from "./rehome";
import { resetLinkRtt } from "./rtt";
import { noteDaemonLink } from "../shared/editing";
import { asyncDisposer } from "../shared/asyncDisposer";

/** What App owns that the connection reads or calls back into. */
export interface ConnectionDeps {
  activeWsId: () => string | null;
  detachedWindow: () => boolean;
  windowLabel: () => string;
  eventsSocket: () => EventsSocket | null;
  /** The events socket came back after being down (not its first connect). */
  onEventsRecovered: () => void;
  /** Native only: the shell's cached update outcome (read at mount and after
   *  every app-update broadcast). The update store lives in a feature area;
   *  net/ hands the facts up instead of importing it. */
  onAppStatus: (status: AppUpdateStatus) => void;
  /** Native only: an app-update broadcast carrying the new version. */
  onAppVersion: (version: string) => void;
  /** The applied appearance, carried in the re-home URL so the new document
   *  paints in the right theme before its settings load (settings area). */
  appearanceForNavigation: () => unknown;
  /** This window's layout key (`layout/viewState.ts` `windowKey`), carried in
   *  the re-home URL so the new document restores the same layout. */
  windowKey: () => string;
  /** The asset-transition bridge (`layout/assetTransition.ts`): the build
   *  source this document was served from, how a health build id maps to a
   *  source, and how to demand a new interface load. */
  assets: {
    servedBuild: () => string | null;
    buildSource: (build: string | null | undefined) => string | null;
    requireNavigation: (reason: "build" | "connection", target: string | null) => void;
  };
}

/** The sessions + links fallback poll while `/ws/events` is down. The
 *  pollers live in the workspace area (`sessions.ts`, `agentLinks.ts`); App
 *  passes them in so net/ depends on no feature area. */
export interface FallbackPoll<S, L> {
  /** `workspace/sessions.ts` `pollSessions`: returns its stop function. */
  sessions: (onResult: (list: S) => void, onError: (e: unknown) => void) => () => void;
  listLinks: () => Promise<L>;
  applySessions: (list: S) => void;
  setLinks: (links: L) => void;
}

/** Minimum spacing between releases of the 401 latch. A daemon that keeps
 *  rejecting this token while the shell keeps reporting `connected` would
 *  otherwise loop 401 → reconnect → release → 401; with the damp an
 *  episode gets at most two automatic rounds, then the latch parks with
 *  the ambient Retry until a manual retry. */
const UNAUTHORIZED_REARM_MS = 30_000;

export class Connection {
  // --- remote window identity (fixed for the document's life) --------------
  /** This window's host alias ("local" for the local daemon). */
  readonly hostAlias = getHostLabel();
  readonly isRemoteWindow = this.hostAlias !== "local";
  /** Set when this window sits on a compute-node daemon (Mode 2 job). */
  readonly jobCtx: JobContext | null = getJobContext();
  /** The key this window's tunnel reports `host-status` under. A job window
   *  listens ONLY on its composite key — matching the bare login alias made
   *  every login-tunnel blip re-home job windows onto the login daemon
   *  (found live: "opening the session just opens the login host"). */
  readonly statusAlias =
    this.jobCtx !== null
      ? `${this.hostAlias}#job${this.jobCtx.jobId}${this.jobCtx.cws !== null ? `~${this.jobCtx.cws}` : ""}`
      : this.hostAlias;
  /** Only the native shell can re-run ssh; a browser tunnel is the CLI's job. */
  readonly canReconnect = this.isRemoteWindow && isNativeShell();
  /** Native events are not replayed. A stale-token page must not invoke
   *  reconnect until its connected endpoint/token listener is attached. */
  readonly reconnectListener = createReconnectListenerGate();
  /** This window's scope key for the shell registry (null alias = local). */
  readonly scopeAlias: string | null = this.isRemoteWindow
    ? this.jobCtx !== null
      ? this.statusAlias
      : this.hostAlias
    : null;

  // --- reachability ---------------------------------------------------------
  health = $state<Health | null>(null);
  /** Last authenticated HTTP health result. The event stream can reconnect
   *  independently while the daemon and SSH tunnel remain reachable. */
  healthUp = $state(false);
  /** The `/ws/events` socket is open. */
  eventsUp = $state(false);
  /** This app binary's build id (native only); vs health.build = skew. */
  appBuild = $state<string | null>(null);

  // --- remote reconnect -----------------------------------------------------
  /** The reconnect status or failure dialog is showing. */
  showReconnect = $state(false);
  /** A connectHost call is in flight. */
  reconnecting = $state(false);
  /** Last reconnect failure, surfaced with a Retry. */
  reconnectError = $state<string | null>(null);
  /** A job window's job left the queue (the shell's `ended` on its key);
   *  `reason` is Slurm's state, or "stopped" for a stop from the app. */
  jobEnded = $state<{ reason: string | null } | null>(null);
  /** Context from the shell's liveness monitor for the current drop. */
  reconnectReason = $state<string | null>(null);

  readonly #unauthorized = fromStore(unauthorized);
  /** Dismissing a failed reconnect downgrades it to an ambient Retry instead
   *  of removing the only recovery path. Unauthorized native windows never
   *  fall through to the browser-only manual auth overlay. */
  reconnectSurface = $derived(
    selectRemoteReconnectSurface({
      open: this.showReconnect,
      error: this.reconnectError,
      authBlocked: this.canReconnect && this.#unauthorized.current,
    }),
  );

  readonly #deps: ConnectionDeps;
  /** Source identity of the daemon that supplied this document. A different
   *  source means its immutable lazy-chunk namespace changed underneath us. */
  #servedBuild: string | null;
  #handledRemoteUnauthorized = false;
  #lastUnauthorizedReconnectAt: number | null = null;
  #healthPoll: PollHandle | null = null;

  /** Call during App's initialisation: it declares the 401 effect and the
   *  window-scope effects. */
  constructor(deps: ConnectionDeps) {
    this.#deps = deps;
    this.#servedBuild = deps.assets.servedBuild();

    // A 401 in a native remote window usually means the daemon restarted and
    // minted a fresh tunnel token. Let the shell reconnect this exact host and
    // re-home the window; the generic "paste a URL" auth page is only correct
    // for browser tunnels, which cannot re-run ssh themselves.
    $effect(() => {
      if (!this.canReconnect || !this.#unauthorized.current || this.#handledRemoteUnauthorized) return;
      this.#handledRemoteUnauthorized = true;
      untrack(() =>
        this.#beginReconnect(
          "The remote daemon changed credentials; reconnecting will refresh this window.",
        ),
      );
    });

    // `/ws/events` owns its own backoff and is not SSH liveness: it can restart
    // while authenticated HTTP remains healthy. Only the shell's consecutive
    // authenticated health failures (or a 401 above) start SSH recovery, and
    // only its `connected` event finishes that recovery.

    // Keep the shell's window registry current so "open this workspace" raises
    // this window instead of duplicating it (the SPA swaps `ws` client-side, so
    // the shell can't see the change otherwise). The report is SPLIT by
    // urgency: the scope IDENTITY (alias/ws/detached) drives raise-dedup,
    // cross-window drag routing, and the persisted record, so it reports
    // immediately — a debounce there leaves the shell routing on a stale
    // workspace and loses the report entirely on a quick ⌘Q. Only the LABEL
    // (which for a detached window tracks its tab, i.e. an agent's mutating
    // title) is debounced, because each report costs a shell-side tray rebuild.
    $effect(() => {
      if (!isNativeShell()) return;
      const alias = this.scopeAlias;
      const reportedWsId = deps.activeWsId();
      const detached = deps.detachedWindow();
      const label = untrack(() => deps.windowLabel()) || null;
      void reportWindowScope(alias, reportedWsId, label, detached).then(
        () => {
          // A promoted local workbench becomes the singleton Home again when
          // its workspace disappears. Restore the browser half of that identity
          // only while the reported empty scope is still current: an older IPC
          // completion must not undo a newer workspace promotion.
          if (alias === null && reportedWsId === null && untrack(() => deps.activeWsId()) === null) {
            reclaimHomeHub();
          }
        },
        () => {
          // Navigation can transiently reject reconciliation; only a confirmed
          // report is allowed to reclaim the browser's Home identity.
        },
      );
    });
    $effect(() => {
      if (!isNativeShell()) return;
      const label = deps.windowLabel() || null;
      const timer = setTimeout(() => {
        void reportWindowScope(
          this.scopeAlias,
          untrack(() => deps.activeWsId()),
          label,
          untrack(() => deps.detachedWindow()),
        ).catch(() => {
          // label-only refresh; the identity effect above is the authority
        });
      }, 250);
      return () => clearTimeout(timer);
    });
  }

  /** The daemon now serving this window is a different build than the one
   *  that served this document. */
  daemonBuildChanged(next: string | null | undefined): boolean {
    const nextSource = this.#deps.assets.buildSource(next);
    return this.#servedBuild !== null && nextSource !== null && nextSource !== this.#servedBuild;
  }

  /** Carry hub identity across daemon origins. `sessionStorage` remembers the
   *  original launcher, while the current local-empty scope covers a promoted
   *  workbench that the shell is reclaiming as Home right now. */
  #shouldCarryHomeHub(): boolean {
    return (
      isHomeHub() || (isNativeShell() && this.scopeAlias === null && this.#deps.activeWsId() === null)
    );
  }

  // --- remote reconnect -----------------------------------------------------

  /** Re-establish this remote window's ssh tunnel. Idempotent: the shell
   *  reuses a live tunnel, and reuses the old loopback port so a heal needs no
   *  navigation. Surfaces the SSH auth modal (mounted app-wide) only if ssh
   *  actually re-prompts. A job window heals through its cluster workspace —
   *  `cluster_open` reuses or rebuilds the job's forward, keeps this window,
   *  and pings the composite status key with the (possibly new) port. */
  async #attemptReconnect(): Promise<void> {
    if (!this.canReconnect || this.reconnecting || this.jobEnded !== null) return;
    this.reconnecting = true;
    this.reconnectError = null;
    try {
      await this.reconnectListener.ready;
      if (this.jobCtx !== null) {
        if (this.jobCtx.cws === null) {
          // A window from before job windows named their workspace: nothing
          // here can find its job again.
          throw new Error(`Open this workspace again from ${this.hostAlias}'s page in Home.`);
        }
        await clusterOpen(this.hostAlias, this.jobCtx.cws);
      } else {
        const failure = connectResultError(await connectHost(this.hostAlias), this.hostAlias);
        if (failure !== null) throw new Error(failure);
      }
      // Every successful shell connect republishes `host-status: connected`,
      // which clears the overlay and re-homes this window when needed.
    } catch (e) {
      this.reconnectError = e instanceof Error ? e.message : String(e);
    } finally {
      this.reconnecting = false;
    }
  }

  #beginReconnect(reason: string): void {
    // A job that ended has nothing to reconnect to: its overlay says so.
    if (!this.canReconnect || this.jobEnded !== null) return;
    this.reconnectReason = reason;
    this.showReconnect = true;
    void this.#attemptReconnect();
  }

  retryReconnect(): void {
    this.showReconnect = true;
    void this.#attemptReconnect();
  }

  dismissReconnect(): void {
    this.showReconnect = false;
  }

  #finishReconnect(): void {
    this.showReconnect = false;
    this.reconnectError = null;
    this.reconnectReason = null;
  }

  #handleHostStatus(e: HostStatusEvent): void {
    const action = hostStatusAction(e, {
      canReconnect: this.canReconnect,
      statusAlias: this.statusAlias,
      hostAlias: this.hostAlias,
      isJobWindow: this.jobCtx !== null,
      jobEnded: this.jobEnded !== null,
      port: location.port,
      token: getToken(),
    });
    switch (action.kind) {
      case "ignore":
        return;
      case "jobEnded":
        this.jobEnded = { reason: action.reason };
        this.#finishReconnect();
        return;
      case "reconnect":
        this.#beginReconnect(action.reason);
        return;
      case "stranded":
        this.reconnectReason = null;
        this.reconnectError = action.message;
        this.showReconnect = true;
        return;
      case "healed":
        break;
    }
    this.#finishReconnect();
    const buildMoved = this.daemonBuildChanged(e.build);
    if (action.moved) {
      this.#deps.assets.requireNavigation(
        buildMoved ? "build" : "connection",
        rehomeUrl({
          port: action.port,
          token: action.token,
          win: this.#deps.windowKey(),
          ws: this.#deps.activeWsId(),
          hub: this.#shouldCarryHomeHub(),
          host: this.hostAlias,
          appearance: this.#deps.appearanceForNavigation(),
          job: this.jobCtx,
        }),
      );
    } else {
      // Neither port nor token moved: the heal was IN PLACE and nothing
      // navigates, so this is the one path that must release a latched 401
      // itself. A moved token stays dead until the reload lands — blocked on
      // unsaved work or not — so releasing there would only burn a slot on a
      // doomed reconnect.
      this.#closeUnauthorizedReconnect();
      if (buildMoved) this.#deps.assets.requireNavigation("build", null);
    }
  }

  /** The shell reported `connected` on the SAME port + token while a 401
   *  was latched: an in-place heal, which never navigates, so the latch
   *  must be released here — or the "needs fresh credentials" chip outlives
   *  a working window — and the one-shot above re-armed, or the next daemon
   *  restart's 401 gets no automatic recovery. Only `handleHostStatus`'s
   *  no-move branch calls this; a moved token is dead until its reload. */
  #closeUnauthorizedReconnect(): void {
    if (!get(unauthorized)) return;
    const now = Date.now();
    if (
      this.#lastUnauthorizedReconnectAt !== null &&
      now - this.#lastUnauthorizedReconnectAt < UNAUTHORIZED_REARM_MS
    ) {
      return;
    }
    this.#lastUnauthorizedReconnectAt = now;
    clearUnauthorized();
    this.#handledRemoteUnauthorized = false;
  }

  // --- effects App declares at their original places ------------------------

  /** Health polling keeps the hostname fresh, trips the 401 overlay, and
   *  independently tracks authenticated daemon reachability. The events
   *  socket can restart while HTTP remains healthy, so the Home badge accepts
   *  either successful channel instead of calling that transient "offline".
   *  Cadence (net/poll.ts): the events socket is the real liveness signal, so
   *  while it's up this is a 60s safety net; while it's down the 5s recovery
   *  probe runs only in a visible window (hidden takes the slow tier, with a
   *  catch-up fetch on visibility return). The delay getter reads `eventsUp`
   *  live at each arm — the poller itself is armed once (no teardown churn).
   *
   *  Declares two effects, the poll first: the kick effect reads the handle
   *  the poll effect assigns. */
  trackHealth(): void {
    $effect(() => {
      const handle = pollHealth(
        (h) => {
          this.healthUp = true;
          // Cross-nudge: HTTP just proved the daemon reachable — if the events
          // socket is sitting out a backoff (or gave up as fatal), retry it
          // now instead of letting two independent slow clocks ignore each
          // other. It in turn nudges the per-session sockets on recovery.
          if (!this.eventsUp) this.#deps.eventsSocket()?.retryNow();
          if (this.daemonBuildChanged(h.build)) {
            // The origin can stay stable across a daemon handoff, but the
            // hashed JS namespace cannot. Reload before the user opens a lazy
            // surface whose old URL now correctly 404s.
            this.#deps.assets.requireNavigation("build", null);
            return;
          }
          this.#servedBuild ??= this.#deps.assets.buildSource(h.build);
          this.health = h;
        },
        () => {
          this.healthUp = false;
        },
        (hidden) => healthPollDelayMs(this.eventsUp, hidden),
      );
      this.#healthPoll = handle;
      return () => {
        this.#healthPoll = null;
        handle.stop();
      };
    });

    // An events up/down transition warrants a prompt reachability probe —
    // kick() is deferred and damped (KICK_DAMP_MS), so a crash-looping daemon
    // flapping the socket once a second cannot become a fetch per transition.
    $effect(() => {
      void this.eventsUp;
      this.#healthPoll?.kick();
    });
  }

  /** Daemon/app build skew (native): the daemon serving this window is a
   *  different build than the app — the update toast offers the restart.
   *  Health rides a poll, so this stays current without extra traffic. The
   *  build-timestamp suffix is ignored, like core's builds_match: the same
   *  commit built at different moments must not read as skew. */
  trackBuildSkew(setSkew: (skew: boolean) => void): void {
    $effect(() => {
      setSkew(
        this.appBuild !== null &&
          typeof this.health?.build === "string" &&
          this.health.build.split(".")[0] !== this.appBuild.split(".")[0],
      );
    });
  }

  /** /ws/events pushes full session snapshots; the 5s poll only runs as a
   *  fallback while the socket is down (including before the first frame).
   *  Links normally ride the socket frames; the fallback refreshes them too. */
  pollWhileEventsDown<S, L>(poll: FallbackPoll<S, L>): void {
    $effect(() => {
      if (this.eventsUp) return;
      return poll.sessions(
        (list) => {
          poll.applySessions(list);
          poll.listLinks().then(poll.setLinks, () => {
            // stale links until the daemon is reachable again
          });
        },
        () => {
          // transient poll failure; the daemon dot already reflects reachability
        },
      );
    });
  }

  // --- wiring App's onMount calls -------------------------------------------

  /** The events socket's `onStatus`. */
  noteEventsStatus(up: boolean): void {
    // A recovered events socket often means a re-established tunnel — a
    // different link. Drop the RTT window so the rolling minimum can't
    // latch the old link's floor (the badge goes blank until the next
    // health sample, which the recovery kick fetches promptly).
    if (up && !this.eventsUp) resetLinkRtt();
    // No comms frame is sent on connect: refetch what changed while the
    // socket was down (or a restarted daemon renumbered).
    if (up && !this.eventsUp) this.#deps.onEventsRecovered();
    this.eventsUp = up;
    // A save that died with the link retries once it is back.
    noteDaemonLink(up);
  }

  /** Native only. Build-skew + app-update signals for the update toast. */
  listenAppUpdate(): () => void {
    void shellBuild().then((b) => (this.appBuild = b));
    // The broadcast carries only the version; the shell's cache holds the
    // whole outcome (when it checked), which Settings → Updates shows. A
    // window opened after a broadcast reads the same cache at mount.
    const readAppStatus = (): void => {
      void appUpdateStatus(false).then(
        (status) => {
          if (status !== null) this.#deps.onAppStatus(status);
        },
        () => {},
      );
    };
    const unlisten = asyncDisposer(
      onAppUpdate((version) => {
        this.#deps.onAppVersion(version);
        readAppStatus();
      }),
    );
    readAppStatus();
    return unlisten;
  }

  /** Native only. The local daemon was replaced (self-update). Re-home
   *  (carrying the window id, so the layout follows) when its origin moved;
   *  when the handoff retained port+token, a changed build still requires a
   *  reload because the immutable hashed asset namespace changed. */
  listenLocalDaemonUpdated(): () => void {
    return asyncDisposer(
      onLocalDaemonUpdated(({ port, token, build }) => {
        if (getHostLabel() !== "local") return;
        if (String(port) === location.port && token === getToken()) {
          if (this.daemonBuildChanged(build)) this.#deps.assets.requireNavigation("build", null);
          return;
        }
        this.#deps.assets.requireNavigation(
          "build",
          rehomeUrl({
            port,
            token,
            win: this.#deps.windowKey(),
            ws: this.#deps.activeWsId(),
            hub: this.#shouldCarryHomeHub(),
            host: null,
            appearance: this.#deps.appearanceForNavigation(),
            job: null,
          }),
        );
      }),
    );
  }

  /** Native only. This remote window's tunnel dropped or came back. "down" →
   *  reconnect now (the shell confirmed the forward is dead); "connected" →
   *  re-home if the origin moved, or reload if only the daemon build moved. A
   *  pure same-build heal stays in place and its WebSocket just reconnects.
   *  Matching is on statusAlias: a job window heeds ONLY its composite key's
   *  events — the login alias's port/token are another daemon. */
  listenHostStatus(): () => void {
    const listening = onHostStatus((e) => this.#handleHostStatus(e));
    void listening.then(this.reconnectListener.attached, this.reconnectListener.failed);
    return asyncDisposer(listening);
  }
}
