import { init, parse } from "es-module-lexer";
import { isRecord, type NativeUiTransport, type UiRecord } from "./nativeUi";

let workers = 0;
export function reserveClientWorker(): (() => void) | null {
  if (workers >= 16) return null;
  workers++;
  let released = false;
  return () => { if (!released) { released = true; workers--; } };
}
const bundles = new WeakMap<NativeUiTransport, Map<string, Promise<UiRecord>>>();
export async function clientBundle(transport: NativeUiTransport, plugin: string, hash: string, module: string): Promise<ClientBundle> {
  let cache = bundles.get(transport);
  if (!cache) { cache = new Map(); bundles.set(transport, cache); }
  const key = `${plugin}:${hash}`;
  let request = cache.get(key);
  if (!request) {
    request = transport.request({ subtype: "ui_client_module", plugin });
    cache.set(key, request);
    while (cache.size > 4) cache.delete(cache.keys().next().value!);
    request.catch(() => { cache?.delete(key); });
  }
  const value = await request;
  if (value.hash !== hash) { cache.delete(key); throw new Error("Claude Mod modules changed; refresh the pane"); }
  return prepareClientBundle(value, module);
}

export interface ClientBundle {
  runtime: string;
  entry: string;
  component: string;
  limits: UiRecord;
  files: { key: string; source: string; imports: { start: number; end: number; key: string; dynamic: boolean }[] }[];
}

export function clientKeyEvent(event: { key: string; ctrlKey: boolean; shiftKey: boolean; metaKey: boolean; altKey: boolean }): UiRecord {
  const names: Record<string, string> = { " ": "space", ArrowUp: "up", ArrowDown: "down", ArrowLeft: "left", ArrowRight: "right", Enter: "return", Tab: "tab", Backspace: "backspace", Delete: "delete", PageUp: "pageup", PageDown: "pagedown", Home: "home", End: "end", Escape: "escape" };
  return { key: names[event.key] ?? event.key, ...(event.ctrlKey ? { ctrl: true } : {}), ...(event.shiftKey ? { shift: true } : {}), ...(event.metaKey || event.altKey ? { meta: true } : {}) };
}

export function clientPointerEvent(type: "down" | "move" | "up" | "enter" | "leave", event: { clientX: number; clientY: number; button: number; buttons: number; shiftKey: boolean; altKey: boolean; ctrlKey: boolean }, geometry: { left: number; top: number; cellWidth: number; cellHeight: number }, previous = { x: 0, y: 0 }): UiRecord {
  const fine = { x: (event.clientX - geometry.left) / Math.max(1, geometry.cellWidth), y: (event.clientY - geometry.top) / Math.max(1, geometry.cellHeight) };
  const crossing = type === "enter" || type === "leave";
  const button = type === "move" ? (event.buttons & 1 ? "left" : event.buttons & 4 ? "middle" : event.buttons & 2 ? "right" : undefined) : type === "down" || type === "up" ? ["left", "middle", "right"][event.button] : undefined;
  return { type, x: crossing ? previous.x : Math.floor(fine.x), y: crossing ? previous.y : Math.floor(fine.y), ...(!crossing ? { fine } : {}), ...(button ? { button } : {}), ...(event.shiftKey ? { shift: true } : {}), ...(event.altKey ? { alt: true } : {}), ...(event.ctrlKey ? { ctrl: true } : {}) };
}

/** Resolve only the runtime's supplied graph. A Mod cannot import code from the workbench or network. */
export async function prepareClientBundle(value: UiRecord, module: string): Promise<ClientBundle> {
  if (!Array.isArray(value.files) || value.files.length > 512 || !Array.isArray(value.modules)) throw new Error("Invalid Claude Mod module bundle");
  const entry = value.modules.find((item) => isRecord(item) && item.module === module);
  if (!isRecord(entry) || typeof entry.entry !== "string" || typeof entry.component !== "string" || typeof value.runtime !== "string") throw new Error("Claude Mod module is unavailable");
  let bytes = 0;
  const files = value.files.map((file) => {
    if (!isRecord(file) || typeof file.key !== "string" || typeof file.source !== "string" || file.source.length > 1024 * 1024) throw new Error("Invalid Claude Mod module file");
    bytes += file.source.length;
    return { key: file.key, source: file.source };
  });
  if (bytes > 8 * 1024 * 1024) throw new Error("Claude Mod modules exceed their size limit");
  const keys = new Set(files.map((file) => file.key));
  if (keys.size !== files.length || !keys.has(value.runtime) || !keys.has(entry.entry)) throw new Error("Invalid Claude Mod module graph");
  await init;
  const linked = files.map((file) => ({ ...file, imports: parse(file.source)[0].filter((item) => item.d !== -2).map((item) => {
    if (!item.n || !keys.has(item.n)) throw new Error("Claude Mod module imports code outside its supplied bundle");
    return { start: item.s, end: item.e, key: item.n, dynamic: item.d >= 0 };
  }) }));
  const visiting = new Set<string>();
  const visited = new Set<string>();
  const graph = new Map(linked.map((file) => [file.key, file]));
  function visit(key: string): void {
    if (visiting.has(key)) throw new Error("This Claude Mod has circular module imports; its Client surface cannot be loaded yet");
    if (visited.has(key)) return;
    visiting.add(key);
    for (const dependency of graph.get(key)!.imports) visit(dependency.key);
    visiting.delete(key); visited.add(key);
  }
  visit(value.runtime); visit(entry.entry);
  return { runtime: value.runtime, entry: entry.entry, component: entry.component, limits: isRecord(value.limits) ? value.limits : {}, files: linked };
}

/** Runs only in an opaque-origin iframe. Plugin code runs only in its terminable worker. */
export const CLIENT_FRAME = `<!doctype html><meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'unsafe-inline' blob:; worker-src blob:; connect-src 'none'; img-src 'none'; style-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'"><script>
let worker;
addEventListener('message', function boot(event) {
  if (event.source !== parent || !event.ports[0]) return;
  removeEventListener('message', boot);
  const port = event.ports[0];
  // Classic bootstrap also works in opaque-origin WebKit/Chromium frames;
  // the supplied modules still load through native dynamic import inside it.
  worker = new Worker(URL.createObjectURL(new Blob([event.data.worker], {type:'text/javascript'})));
  worker.onmessage = (message) => port.postMessage(message.data);
  worker.onerror = (error) => port.postMessage({type:'error',message:error.message || ('Mod worker could not start' + (error.filename ? ': ' + error.filename + ':' + error.lineno : ''))});
  port.onmessage = (message) => { if (message.data.type === 'stop') { worker.terminate(); port.close(); } else worker.postMessage(message.data); };
  worker.postMessage(event.data.initial);
});
<\/script>`;

// This source contains only the host runner. Bundle code is transferred after
// the opaque frame exists, then loaded as worker-local Blob modules under CSP.
export const CLIENT_WORKER = `
// No nested execution contexts: the host's worker cap remains effective.
for (const name of ['Worker','SharedWorker','BroadcastChannel','WebSocket','EventSource','fetch','XMLHttpRequest','importScripts']) {
  try { Object.defineProperty(globalThis,name,{value:undefined,writable:false,configurable:false}); } catch {}
}
let runtime, bundle, held = [], scheduled = false, timerSerial = 0;
const pinned = new Set();
const timers = new Map();
const send = (message) => postMessage(message);
const execute = (kind, payload) => {
  send({type:'execution',active:true});
  try { runtime.stage('client', kind, payload); return globalThis.__surface__.run(); }
  finally { send({type:'execution',active:false}); }
};
const render = () => {
  scheduled = false;
  const text = execute('render');
  if (typeof text !== 'string' || text.length > 1000000) throw Error('Mod render exceeded its size limit');
  const tree = JSON.parse(text), next = [];
  const walk = (node) => { if (!node || typeof node !== 'object') return; if (Number.isInteger(node.held)) next.push(node.held); if (Array.isArray(node.children)) node.children.forEach(walk); };
  walk(tree); runtime.dropHeld('client', held.filter(handle => !pinned.has(handle))); held = next;
  send({type:'tree',tree,listeners:{key:runtime.hasListener('client','key'),pointer:runtime.hasListener('client','pointer')}});
};
const schedule = () => { if (!scheduled) { scheduled = true; setTimeout(() => { try { render(); } catch (error) { send({type:'error',message:String(error)}); } }, 34); } };
onmessage = async ({data}) => {
  const id = data.id;
  try {
    if (data.type === 'init') {
      bundle = data.bundle;
      const files = new Map(bundle.files.map(file => [file.key,file])), urls = new Map();
      const link = key => {
        if (urls.has(key)) return urls.get(key);
        const file = files.get(key); let source = file.source;
        for (const ref of [...file.imports].reverse()) { const url = link(ref.key); source = source.slice(0, ref.start) + (ref.dynamic ? JSON.stringify(url) : url) + source.slice(ref.end); }
        const url = URL.createObjectURL(new Blob([source],{type:'text/javascript'})); urls.set(key,url); return url;
      };
      const support = await import(link(bundle.runtime));
      globalThis.h = support.h; globalThis.Fragment = support.Fragment;
      const host = {
        schedule,
        startTimer: (_, ms) => {
          if (timers.size >= 32) throw Error('Mod timer limit reached');
          const timer = ++timerSerial;
          timers.set(timer, setInterval(() => { try { execute('tick',timer); schedule(); } catch(error) { send({type:'error',message:String(error)}); } }, Math.max(100, ms)));
          return timer;
        },
        stopTimer: (_, timer) => { clearInterval(timers.get(timer)); timers.delete(timer); },
        post: (_, text) => { if (text.length > 100000) throw Error('Mod message too large'); send({type:'post',data:JSON.parse(text)}); }
      };
      runtime = support.install(host, { ...bundle.limits, nodes: Math.min(4096, bundle.limits.nodes || 4096), chars: Math.min(1000000, bundle.limits.chars || 100000), depth: Math.min(32,bundle.limits.depth || 32) });
      send({type:'execution',active:true});
      const module = await import(link(bundle.entry));
      send({type:'execution',active:false});
      runtime.mount('client', module[bundle.component], data.props);
      runtime.resize('client', data.columns || 80, data.rows || 24);
      render();
    } else if (data.type === 'props') { runtime.setProps('client',data.props); render(); }
    else if (data.type === 'resize') { runtime.resize('client',data.columns,data.rows); render(); }
    else if (data.type === 'pin') { if (!held.includes(data.handle)) { send({type:'response_error',id,message:'This control changed; try its refreshed version'}); return; } if (pinned.size >= 32) throw Error('Mod control limit reached'); pinned.add(data.handle); }
    else if (data.type === 'unpin') { pinned.delete(data.handle); if (!held.includes(data.handle)) runtime.dropHeld('client',[data.handle]); }
    else if (data.type === 'held') { if (!held.includes(data.handle) && !pinned.has(data.handle)) throw Error('This control changed; try its refreshed version'); execute('held',{handle:data.handle,event:data.event}); schedule(); }
    else if (data.type === 'key' || data.type === 'pointer') { if (runtime.hasListener('client',data.type)) execute(data.type,data.event); }
    send({type:'ack',id});
  } catch(error) { send({type:'error',message:String(error),id}); }
};
`;
