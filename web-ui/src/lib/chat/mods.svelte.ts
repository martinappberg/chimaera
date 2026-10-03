import { isRecord, parseUiTree, type NativeUiNotice, type NativeUiTransport, type UiNode, type UiRecord } from "./nativeUi";
import { copyText } from "../shared/clipboard";
import type { NativeComposerHost } from "./nativeComposer";

export interface ModPane { id: string; title: string; plugin: string; close_on_escape?: boolean; hold_toasts?: boolean }
export interface ModSite { component: string; instance_id: string; props: UiRecord; viewport?: { columns: number; rows: number; isFullscreen: boolean }; content_rows?: number; keyed?: { plugin: string; key: string; top: number; bottom: number }[] }
export interface ModRender { tree: UiNode | null; error: string | null; clientModules: UiRecord; props: UiRecord }
interface Registration { site: ModSite; notify: (render: ModRender) => void; pending: boolean; dirty: boolean; generation: number }

/** One attach per websocket, even when the same chat is visible in two workbench panes. */
export class ModsController {
  attached = $state(false);
  error = $state<string | null>(null);
  panes = $state<ModPane[]>([]);
  shown = $state<string | null>(null);
  focusRequested = $state<string | null>(null);
  focused = $state<string | null>(null);
  statuses = $state<{ plugin: string; text: string }[]>([]);
  toast = $state<{ text: string; plugin: string } | null>(null);
  private active = 0;
  private generation = 0;
  private registrations = new Set<Registration>();
  private unsubscribe: (() => void) | null = null;
  private toastTimer: ReturnType<typeof setTimeout> | null = null;
  private listeners = new Set<(payload: UiRecord) => void>();
  private rendersInFlight = 0;
  private renderWaiters: (() => void)[] = [];
  private hosts = new Set<() => NativeComposerHost>();

  constructor(readonly transport: NativeUiTransport) {}

  retain(): () => void {
    this.active++;
    if (this.active === 1) {
      this.unsubscribe = this.transport.subscribe((event) => this.onNotice(event));
      if (this.transport.ready) void this.attach();
    }
    return () => {
      if (--this.active !== 0) return;
      ++this.generation;
      this.attached = false;
      this.unsubscribe?.(); this.unsubscribe = null;
      if (this.toastTimer) clearTimeout(this.toastTimer);
      this.toast = null;
      if (this.transport.ready) void this.transport.request({ subtype: "ui_detach" }).catch(() => {});
      this.clearTrees();
    };
  }

  subscribe(listener: (payload: UiRecord) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  registerHost(host: () => NativeComposerHost): () => void {
    this.hosts.add(host);
    return () => this.hosts.delete(host);
  }

  private async hostRequest(payload: UiRecord): Promise<void> {
    if (!isRecord(payload.request) || typeof payload.request_id !== "string") return;
    const request = payload.request;
    const hosts = [...this.hosts].map((host) => host());
    const host = hosts.find((item) => item.focused) ?? hosts[0];
    let response: UiRecord;
    switch (request.subtype) {
      case "ui_copy": response = { copied: typeof request.text === "string" && await copyText(request.text) }; break;
      case "ui_prompt_read": response = host?.composer?.nativeRead() ?? { text: "", cursor: 0 }; break;
      case "ui_prompt_fill": response = { filled: !!host?.canEdit && typeof request.text === "string" && (host.composer?.nativeFill(request.text, String(request.mode ?? "replace"), request.decorations) ?? false) }; break;
      case "ui_prompt_suggest": response = { shown: !!host?.canEdit && typeof request.text === "string" && (host.composer?.nativeSuggest(request.text) ?? false) }; break;
      default: return;
    }
    await this.transport.request({ subtype: "ui_host_response", request_id: payload.request_id, response }).catch(() => {});
  }

  private clearTrees(): void {
    for (const registration of this.registrations) {
      ++registration.generation;
      registration.pending = false;
      registration.notify({ tree: null, error: null, clientModules: {}, props: registration.site.props });
    }
  }

  async attach(): Promise<void> {
    const generation = ++this.generation;
    this.attached = false;
    this.error = null;
    this.focusRequested = null;
    this.focused = null;
    this.clearTrees();
    try {
      await this.transport.request({ subtype: "ui_attach", answers: ["ui_copy", "ui_prompt_read", "ui_prompt_fill", "ui_prompt_suggest"] });
      if (this.active === 0 || generation !== this.generation) return;
      this.attached = true;
      const roster = await this.transport.request({ subtype: "ui_panes" });
      if (this.active === 0 || generation !== this.generation) return;
      this.roster(roster);
      for (const registration of this.registrations) void this.render(registration);
    } catch (error) {
      if (this.active > 0 && generation === this.generation) this.error = error instanceof Error ? error.message : String(error);
    }
  }

  private onNotice(notice: NativeUiNotice): void {
    if (notice.type === "ready" || notice.type === "reset") { void this.attach(); return; }
    if (notice.type === "disconnected") { ++this.generation; this.attached = false; this.clearTrees(); return; }
    if (notice.type !== "notification") return;
    const event = notice.payload;
    if (event.type === "control_request") { void this.hostRequest(event); return; }
    const type = event.subtype ?? event.type;
    if (type === "ui_panes") this.roster(event);
    if (type === "ui_invalidate") {
      const instances = Array.isArray(event.instances) ? event.instances.filter(isRecord) : null;
      for (const registration of this.registrations) {
        if (instances === null || instances.some((instance) => instance.component === registration.site.component && instance.instance_id === registration.site.instance_id)) void this.render(registration);
      }
    }
    if (type === "ui_status" && typeof event.plugin === "string") {
      const next = this.statuses.filter((status) => status.plugin !== event.plugin);
      if (typeof event.text === "string" && event.text) next.push({ plugin: event.plugin, text: event.text.slice(0, 4096) });
      this.statuses = next.slice(-16);
    }
    if (type === "ui_status_snapshot" && Array.isArray(event.statuses)) this.statuses = event.statuses.filter((status): status is { plugin: string; text: string } => isRecord(status) && typeof status.plugin === "string" && typeof status.text === "string").slice(0, 16);
    if (type === "ui_toast" && typeof event.text === "string") {
      this.toast = { text: event.text.slice(0, 4096), plugin: String(event.plugin ?? "Mod") };
      if (this.toastTimer) clearTimeout(this.toastTimer);
      this.toastTimer = setTimeout(() => { this.toast = null; }, Math.max(1500, Math.min(30_000, Number(event.timeout_ms) || 5000)));
    }
    for (const listener of this.listeners) listener(event);
  }

  private roster(result: UiRecord): void {
    if (Array.isArray(result.panes)) this.panes = result.panes.filter((pane): pane is ModPane => isRecord(pane) && typeof pane.id === "string" && typeof pane.title === "string" && typeof pane.plugin === "string").slice(0, 32);
    this.shown = typeof result.shown_id === "string" ? result.shown_id : null;
    this.focused = typeof result.focused_id === "string" ? result.focused_id : null;
    this.focusRequested = typeof result.focus_requested_id === "string" ? result.focus_requested_id : null;
  }

  mount(site: ModSite, notify: Registration["notify"]): { update: (site: ModSite) => void; refresh: () => void; dispose: () => void } {
    const registration: Registration = { site, notify, pending: false, dirty: false, generation: 0 };
    this.registrations.add(registration);
    if (this.attached) void this.render(registration);
    return {
      update: (next) => { registration.site = next; if (this.attached) void this.render(registration); },
      refresh: () => { if (this.attached) void this.render(registration); },
      dispose: () => { ++registration.generation; this.registrations.delete(registration); },
    };
  }

  private async render(registration: Registration): Promise<void> {
    if (!this.attached) return;
    if (registration.pending) { registration.dirty = true; return; }
    registration.pending = true;
    registration.dirty = false;
    const generation = ++registration.generation;
    if (this.rendersInFlight >= 4) await new Promise<void>((resolve) => this.renderWaiters.push(resolve));
    else this.rendersInFlight++;
    try {
      if (!this.attached || generation !== registration.generation || !this.registrations.has(registration)) return;
      const result = await this.transport.request({ subtype: "ui_render", ...registration.site });
      if (generation !== registration.generation || !this.registrations.has(registration)) return;
      registration.notify({ tree: result.hooked === false ? null : parseUiTree(result.tree), error: null, clientModules: isRecord(result.client_modules) ? result.client_modules : {}, props: isRecord(result.props) ? result.props : registration.site.props });
    } catch (error) {
      if (generation === registration.generation && this.attached) registration.notify({ tree: null, error: error instanceof Error ? error.message : String(error), clientModules: {}, props: registration.site.props });
    } finally {
      const next = this.renderWaiters.shift();
      if (next) next();
      else this.rendersInFlight--;
      if (generation === registration.generation) {
        registration.pending = false;
        if (registration.dirty && this.registrations.has(registration)) void this.render(registration);
      }
    }
  }

  async paneAction(subtype: "ui_close" | "ui_pane_show" | "ui_pane_focus", id: string | null): Promise<void> {
    try {
      await this.transport.request({ subtype, id });
      this.roster(await this.transport.request({ subtype: "ui_panes" }));
    } catch (error) { this.error = error instanceof Error ? error.message : String(error); }
  }
}

const controllers = new WeakMap<NativeUiTransport, ModsController>();
export function modsFor(transport: NativeUiTransport): ModsController {
  let controller = controllers.get(transport);
  if (!controller) { controller = new ModsController(transport); controllers.set(transport, controller); }
  return controller;
}
