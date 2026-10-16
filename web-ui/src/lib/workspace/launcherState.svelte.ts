/**
 * The rail's new-agent split button and its launcher popover: the saved
 * default agent, the host's agent catalog, the one quick-start row it
 * derives, the popover's open/anchor/hover-intent state, and what a pick,
 * install or update does.
 *
 * App constructs one `LauncherState` and keeps the spawn itself
 * (`spawnSession` writes the roster and the layout, which only App assigns):
 * every start goes through the `startAgent` callback. App's catalog effect
 * hands fresh catalogs in through `adoptCatalog`, at its original place in
 * App's effect order. Timer duration (150 ms hover intent) is as before.
 */
import { requestSettingsSection } from "../settings/jump";
import { getSetting, setSetting } from "../settings/store.svelte";
import { agentProductName } from "../shared/agentNames";
import { openAgentSetup } from "./agentSetup";
import {
  getAgentDefault,
  listAgents,
  primaryStart,
  recheckAgent,
  setAgentDefault,
  type AgentInfo,
  type LaunchPick,
  type PrimaryStart,
} from "./launcher";
import type { AgentSpawn } from "./sessions";

export interface LauncherDeps {
  activeWsId: () => string | null;
  /** Start an agent (App's spawn path). */
  startAgent: (spawn: AgentSpawn) => void;
  /** No workspace yet: the folder picker. */
  openPicker: () => void;
  /** Open/focus the Settings surface (the section is chosen here). */
  openSettingsSurface: () => void;
  /** An outside close hands focus back to the focused terminal. */
  refocusTerminal: () => void;
}

/** Hover intent on the chevron before the popover opens. */
const HOVER_INTENT_MS = 150;

export class LauncherState {
  /** The persisted default agent the split button's main surface spawns. */
  agentDefault = $state(getAgentDefault());
  /** The host's agent catalog (install status per CLI), so the split button
   *  can reflect whether its default is installed. Null until first fetched;
   *  the launcher popover reports its fresher probe back via `setAgents`. */
  agents = $state<AgentInfo[] | null>(null);
  /** The rail's one new-agent row: the saved default, which the main click
   *  and the new-agent shortcut start; its chevron opens the launcher menu.
   *  It knows what its click does: start, or the setup step that unblocks a
   *  missing agent (launcher.ts `primaryStart`). */
  readonly railStart = $derived(
    primaryStart(
      this.agents,
      this.agentDefault.agent,
      getSetting("agents.defaultView"),
      agentProductName,
    ),
  );
  open = $state(false);
  /** The split button's rect at open time; the popover hangs below it. */
  anchor = $state<DOMRect | null>(null);
  /** The split button; App's markup binds it (`bind:this`), the one write
   *  to this class's state from outside its methods. */
  splitEl = $state<HTMLElement | null>(null);
  /** When the popover opened, so a chevron click landing right after a
   *  hover-open confirms it instead of flash-closing it. */
  openedAt = 0;

  /** Hover-intent timer (~150ms, chevron only) that opens the launcher. */
  #hoverTimer: ReturnType<typeof setTimeout> | null = null;
  readonly #deps: LauncherDeps;

  constructor(deps: LauncherDeps) {
    this.#deps = deps;
  }

  /** A non-empty catalog from the shared store. */
  adoptCatalog(catalog: AgentInfo[]): void {
    if (catalog.length) this.agents = catalog;
  }

  /** The popover's fresher probe. */
  setAgents = (a: AgentInfo[]): void => {
    this.agents = a;
  };

  /** One rail quick start: spawn that agent instantly in the default view
   *  (`term` = the ⌘/ctrl-click terminal path) — or, when it could only open
   *  a doomed pane, the step that unblocks it: a retry of a check the login
   *  shell couldn't finish, the curated install / chat setup (managed
   *  runtimes) or Settings → Agents. A quick start does NOT change the saved
   *  default: the rows never reorder under the pointer; choosing in the
   *  launcher menu is what moves the primary. */
  quickStart(q: PrimaryStart, term = false): void {
    this.open = false;
    if (q.action === "check") {
      // Look again only: a start that lands half a minute later would
      // surprise. The row reads ready once the check finds it.
      void recheckAgent(q.id).catch(() => {});
      return;
    }
    if (term && q.info?.installed !== false) {
      this.#deps.startAgent({ agent: q.id, ui: "term" });
      return;
    }
    if ((q.action === "install" || q.action === "setup-chat") && q.info !== null) {
      this.install(q.info);
      return;
    }
    if (q.action === "setup") {
      this.openAgentSettings();
      return;
    }
    this.#deps.startAgent({ agent: q.id });
  }

  /** The rail's row / Cmd+Shift+E: the saved default, instantly (or its
   *  setup step when it isn't runnable here). */
  newAgentPrimary = (): void => {
    this.quickStart(this.railStart);
  };

  /** Settings → Agents: where every agent is installed, updated and pointed
   *  at a binary (the launcher's footer, a set-up chip). */
  openAgentSettings = (): void => {
    this.open = false;
    this.#deps.openSettingsSurface();
    requestSettingsSection("Agents");
  };

  /** Re-probe the host's agent catalog. `force` bypasses the daemon's
   *  detection cache (after an install, or when the launcher opens). Failures
   *  keep the last-known catalog rather than blanking the button. */
  async refreshAgents(force = false): Promise<void> {
    try {
      this.agents = await listAgents(force);
    } catch {
      // keep the last catalog; the next trigger retries
    }
  }

  openMenu = (): void => {
    if (this.open || this.#deps.activeWsId() === null || this.splitEl === null) return;
    this.anchor = this.splitEl.getBoundingClientRect();
    this.openedAt = Date.now();
    this.open = true;
  };

  /** Escape hands focus back to the chevron (menu-button pattern); an
   *  outside press returns it to the focused terminal. */
  closeMenu = (how?: "escape"): void => {
    this.open = false;
    if (how === "escape") {
      this.splitEl?.querySelector<HTMLElement>(".new-chev")?.focus();
      return;
    }
    this.#deps.refocusTerminal();
  };

  /** Hover intent (~150ms) on the CHEVRON opens the launcher — the main
   *  surface never does; it is a pure instant spawn (field feedback: a
   *  popover chasing every hover of the button read as intrusive). */
  armHover = (): void => {
    if (this.open || this.#deps.activeWsId() === null) return;
    this.disarmHover();
    this.#hoverTimer = setTimeout(() => {
      this.#hoverTimer = null;
      this.openMenu();
    }, HOVER_INTENT_MS);
  };

  disarmHover = (): void => {
    if (this.#hoverTimer !== null) {
      clearTimeout(this.#hoverTimer);
      this.#hoverTimer = null;
    }
  };

  /** Every launcher selection becomes the new default agent and spawns. An
   *  EXPLICIT surface pick (the "open"/terminal buttons, ⌘↵) also becomes the
   *  sticky default view, so the split button and the next plain row press
   *  follow it until the user picks the other surface again. */
  pick = (pick: LaunchPick): void => {
    this.open = false;
    this.agentDefault = { agent: pick.agent };
    setAgentDefault(this.agentDefault);
    if (pick.explicit === true && pick.ui !== undefined) {
      // The setting's vocabulary is "terminal"; the wire/pick term is "term".
      setSetting("agents.defaultView", pick.ui === "term" ? "terminal" : "chat");
    }
    this.#deps.startAgent(pick);
  };

  install = (a: AgentInfo): void => {
    this.open = false;
    const ws = this.#deps.activeWsId();
    if (ws === null) { this.#deps.openPicker(); return; }
    openAgentSetup(a, ws, "install");
  };

  update = (a: AgentInfo): void => {
    this.open = false;
    const ws = this.#deps.activeWsId();
    if (ws === null) { this.#deps.openPicker(); return; }
    openAgentSetup(a, ws, "update");
  };
}
