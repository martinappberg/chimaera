/** Ephemeral Claude UI RPCs. Closure handles never enter the journal or a replay queue. */
export type UiRecord = Record<string, unknown>;
export type NativeUiNotice = { type: "ready" | "disconnected" | "reset" } | { type: "notification"; payload: UiRecord };
/** Only controls directly dispatched by a click, edit, or selection carry intent. */
export function isNativeUiAction(request: unknown): boolean {
  return isRecord(request) && ["ui_press", "ui_input", "ui_select", "ui_client_press"].includes(request.subtype as string);
}
const MAX_PENDING = 32;
const REQUEST_TIMEOUT = 20_000;
const MAX_REQUEST_BYTES = 128 * 1024;

export class NativeUiTransport {
  ready = false;
  private serial = 0;
  private pending = new Map<string, { resolve: (value: UiRecord) => void; reject: (reason: Error) => void; timer: ReturnType<typeof setTimeout> }>();
  private listeners = new Set<(notice: NativeUiNotice) => void>();

  constructor(private send: (frame: UiRecord) => boolean) {}

  subscribe(listener: (notice: NativeUiNotice) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  private emit(notice: NativeUiNotice): void {
    for (const listener of this.listeners) listener(notice);
  }

  connected(): void {
    this.ready = true;
    this.emit({ type: "ready" });
  }

  reset(disconnected = false): void {
    if (disconnected) this.ready = false;
    for (const entry of this.pending.values()) {
      clearTimeout(entry.timer);
      entry.reject(new Error(disconnected ? "Claude UI disconnected" : "Claude UI refreshed; try the action again"));
    }
    this.pending.clear();
    this.emit({ type: disconnected ? "disconnected" : "reset" });
  }

  receive(event: unknown): void {
    if (!isRecord(event)) return;
    if (event.kind === "notification" && isRecord(event.payload)) {
      this.emit({ type: "notification", payload: event.payload });
    } else if (event.kind === "response" && typeof event.request_id === "string") {
      const entry = this.pending.get(event.request_id);
      if (!entry) return;
      this.pending.delete(event.request_id);
      clearTimeout(entry.timer);
      if (typeof event.error === "string") entry.reject(new Error(event.error));
      else entry.resolve(isRecord(event.result) ? event.result : {});
    }
  }

  request(request: UiRecord): Promise<UiRecord> {
    if (!this.ready) return Promise.reject(new Error("Claude UI is not connected"));
    if (this.pending.size >= MAX_PENDING) return Promise.reject(new Error("Claude UI is busy; try again"));
    if (new TextEncoder().encode(JSON.stringify(request)).length > MAX_REQUEST_BYTES) return Promise.reject(new Error("Claude UI request is too large"));
    const request_id = `ui-${++this.serial}`;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(request_id);
        reject(new Error("Claude UI did not respond; try again"));
      }, REQUEST_TIMEOUT);
      this.pending.set(request_id, { resolve, reject, timer });
      if (!this.send({ type: "native_ui", request_id, request })) {
        this.pending.delete(request_id);
        clearTimeout(timer);
        reject(new Error("Claude UI disconnected"));
      }
    });
  }
}

export function isRecord(value: unknown): value is UiRecord {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

export interface UiPress { plugin: string; handle: number }
export interface UiNode { type: string; props: UiRecord; children: (UiNode | string)[]; press?: UiPress; client?: { plugin: string }; group?: { plugin: string }; ref?: number; engineOrdinal?: number; held?: number }
const TYPES = new Set(["Box", "Text", "div", "span", "b", "Button", "Input", "Select", "Link", "Code", "Markdown", "Svg", "Client", "engine"]);
/** Separate browser validation keeps malformed upstream output inside its Mod surface. */
export function parseUiTree(input: unknown): UiNode | null {
  let count = 0;
  let chars = 0;
  let engines = 0;
  function parse(value: unknown, depth: number): UiNode | string | null {
    if (++count > 4096 || depth > 40) throw new Error("Claude Mod exceeded its UI size limit");
    if (typeof value === "string") { chars += value.length; if (chars > 1_000_000) throw new Error("Claude Mod exceeded its text limit"); return value; }
    if (!isRecord(value) || typeof value.type !== "string" || !TYPES.has(value.type)) throw new Error("Claude Mod returned an unsupported UI element");
    const props = isRecord(value.props) ? value.props : {};
    chars += JSON.stringify(props).length;
    if (chars > 1_000_000) throw new Error("Claude Mod exceeded its text limit");
    const node: UiNode = { type: value.type, props, children: [] };
    if (value.type === "engine") node.engineOrdinal = engines++;
    if (isRecord(value.press) && typeof value.press.plugin === "string" && Number.isSafeInteger(value.press.handle)) node.press = { plugin: value.press.plugin, handle: value.press.handle as number };
    if (isRecord(value.client) && typeof value.client.plugin === "string") node.client = { plugin: value.client.plugin };
    if (isRecord(value.group) && typeof value.group.plugin === "string") node.group = { plugin: value.group.plugin };
    if (Number.isSafeInteger(value.ref)) node.ref = value.ref as number;
    if (Number.isSafeInteger(value.held)) node.held = value.held as number;
    if (Array.isArray(value.children)) node.children = value.children.map((child) => parse(child, depth + 1)).filter((child): child is UiNode | string => child !== null);
    return node;
  }
  if (input === undefined || input === null) return null;
  const result = parse(input, 0);
  return typeof result === "string" ? { type: "Text", props: {}, children: [result] } : result;
}

export function safeUiHref(value: unknown): string | null {
  if (typeof value !== "string") return null;
  try {
    const url = new URL(value);
    return url.protocol === "https:" || (url.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname)) ? url.href : null;
  } catch { return null; }
}

const COLORS: Record<string, string> = { red: "var(--err)", green: "var(--accent)", yellow: "var(--warn)", blue: "var(--accent)", cyan: "var(--accent)", magenta: "var(--accent)", white: "var(--fg)", black: "var(--bg)", gray: "var(--muted)", grey: "var(--muted)", text: "var(--fg)", dim: "var(--muted)", muted: "var(--muted)", accent: "var(--accent)", success: "var(--accent)", error: "var(--err)", warning: "var(--warn)" };
/** Allowlisted scalar layout only: never accept plugin CSS, URLs, positioning, or selectors. */
export function uiStyle(props: UiRecord, type: string): string {
  const styles: string[] = [];
  const enumerated: Record<string, [string, string[]]> = {
    flexDirection: ["flex-direction", ["row", "column", "row-reverse", "column-reverse"]],
    alignItems: ["align-items", ["flex-start", "flex-end", "center", "stretch"]],
    justifyContent: ["justify-content", ["flex-start", "flex-end", "center", "space-between", "space-around", "space-evenly"]],
    flexWrap: ["flex-wrap", ["wrap", "nowrap"]],
    textAlign: ["text-align", ["left", "center", "right"]],
  };
  if (type === "Box" || type === "div") styles.push("display:flex", "flex-direction:column");
  for (const [key, [property, values]] of Object.entries(enumerated)) if (typeof props[key] === "string" && values.includes(props[key] as string)) styles.push(`${property}:${props[key]}`);
  for (const [key, property] of Object.entries({ gap: "gap", padding: "padding", paddingX: "padding-inline", paddingY: "padding-block", paddingTop: "padding-top", paddingBottom: "padding-bottom", paddingLeft: "padding-left", paddingRight: "padding-right", margin: "margin", marginX: "margin-inline", marginY: "margin-block", marginTop: "margin-top", marginBottom: "margin-bottom" })) {
    if (typeof props[key] === "number" && Number.isFinite(props[key])) styles.push(`${property}:${Math.max(0, Math.min(16, props[key] as number)) * 0.5}em`);
  }
  for (const key of ["flexGrow", "flexShrink"]) if (typeof props[key] === "number") styles.push(`${key === "flexGrow" ? "flex-grow" : "flex-shrink"}:${Math.max(0, Math.min(10, props[key] as number))}`);
  for (const key of ["width", "height", "minWidth", "minHeight", "maxWidth", "maxHeight"]) {
    const value = props[key];
    const property = key.replace(/[A-Z]/g, (char) => `-${char.toLowerCase()}`);
    if (typeof value === "number" && Number.isFinite(value)) styles.push(`${property}:${Math.max(0, Math.min(512, value))}${key.toLowerCase().includes("width") ? "ch" : "lh"}`);
    else if (typeof value === "string" && /^\d{1,3}%$/.test(value) && Number.parseFloat(value) <= 100) styles.push(`${property}:${value}`);
  }
  if (props.bold || type === "b") styles.push("font-weight:600");
  if (props.italic) styles.push("font-style:italic");
  if (props.underline) styles.push("text-decoration:underline");
  if (props.strikethrough) styles.push("text-decoration:line-through");
  if (props.dimColor) styles.push("opacity:.65");
  if (typeof props.color === "string" && COLORS[props.color]) styles.push(`color:${COLORS[props.color]}`);
  if (typeof props.backgroundColor === "string" && COLORS[props.backgroundColor]) styles.push(`background-color:${COLORS[props.backgroundColor]}`);
  if (props.borderStyle) styles.push("border:1px solid var(--edge)", "border-radius:6px", "padding:.5em");
  return styles.join(";");
}
