/**
 * Tab-switch stall harness. Compiled in only when the UI is built with
 * `CHIMAERA_PERF=1` (scripts/perf/tab-switch/README.md) and armed by
 * `?stalldrive=<sessA,sessB>|<file,..>|<log-path>` in the query string — the
 * app rewrites the hash at boot, the query survives. It opens the tabs, parks
 * the documents (scrolled, so the reveal check is meaningful), leaves a caret
 * in the last document and switches away (the parked-caret case), runs
 * isolation probes, cycles the two terminals with the engine's own caret walk
 * timed from the active layer, reveals the first document, and writes every
 * measurement to <log-path> through the daemon.
 */
import { tick } from "svelte";
import { activateTab, findPane, openFile, openSession, type Layout } from "../layout/layout";
import { api } from "../net/api";

export interface DriveHost {
  get(): Layout;
  set(next: Layout): void;
}

export function stallDriveSpec(): string | null {
  return new URLSearchParams(location.search).get("stalldrive");
}

const sleep = (ms: number): Promise<void> => new Promise((r) => setTimeout(r, ms));
const frame = (): Promise<void> =>
  new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(() => r())));
/** Force style + layout so a probe's cost lands inside its timing. */
const force = (): number => document.body.offsetHeight + getComputedStyle(document.body).color.length;

const activeLayer = (): HTMLElement | null => document.querySelector<HTMLElement>(".layer.active");

function edgeText(root: Element, last: boolean): Text | null {
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  let found: Text | null = null;
  while (walker.nextNode()) {
    found = walker.currentNode as Text;
    if (!last) break;
  }
  return found;
}

function scroller(): HTMLElement | null {
  const layer = activeLayer();
  if (layer === null) return null;
  for (const el of layer.querySelectorAll<HTMLElement>("*")) {
    if (el.scrollHeight > el.clientHeight + 50 && getComputedStyle(el).overflowY !== "visible") return el;
  }
  return null;
}

export async function runStallDrive(host: DriveHost, spec: string): Promise<void> {
  const [sessPart = "", filePart = "", logPath = ""] = spec.split("|");
  const lines: string[] = [];
  const log = (line: string): void => {
    lines.push(`${new Date().toISOString()} ${line}`);
  };
  const write = (): Promise<unknown> =>
    api(`/fs/file?path=${encodeURIComponent(logPath)}`, {
      method: "PUT",
      body: lines.join("\n") + "\n",
    }).catch(() => undefined);
  const probe = (label: string, fn: () => void): void => {
    force();
    const t = performance.now();
    fn();
    force();
    log(`probe ${label} ${(performance.now() - t).toFixed(1)}ms`);
  };

  await sleep(1500);
  const paneId = host.get().focusedPaneId;
  for (const id of sessPart.split(",").filter(Boolean)) host.set(openSession(host.get(), id));
  for (const f of filePart.split(",").filter(Boolean)) host.set(openFile(host.get(), f, false));
  await tick();
  const pane = findPane(host.get().root, paneId);
  if (pane === null) return;
  const kinds = pane.tabs.map((t) => t.surface);
  log(`tabs=${kinds.join(",")} pane=${paneId}`);
  const termIdx = kinds.map((k, i) => (k === "terminal" ? i : -1)).filter((i) => i >= 0);
  const fileIdx = kinds.map((k, i) => (k === "file" ? i : -1)).filter((i) => i >= 0);
  const go = async (idx: number): Promise<number> => {
    const t0 = performance.now();
    host.set(activateTab(host.get(), paneId, idx));
    await tick();
    await frame();
    return performance.now() - t0;
  };

  // Park every document, scrolled.
  for (const i of fileIdx) {
    await go(i);
    await sleep(2500);
    const sc = scroller();
    if (sc !== null) {
      sc.scrollTop = 5000;
      await frame();
      log(`park file ${i} scrollTop=${sc.scrollTop}`);
    }
  }
  if (termIdx.length === 0) return;

  // The parked-caret case: a caret left in a document that then parks.
  if (fileIdx.length > 0) {
    const layer = activeLayer();
    const text = layer === null ? null : edgeText(layer, true);
    const sel = document.getSelection();
    if (text !== null && sel !== null) sel.collapse(text, text.length);
    const docToTerm = await go(termIdx[0]);
    log(`document->terminal switch ${docToTerm.toFixed(1)}ms`);
    const anchor = sel?.anchorNode ?? null;
    const inParked = anchor !== null && !(activeLayer()?.contains(anchor) ?? false);
    let walk = -1;
    if (sel !== null && sel.rangeCount > 0 && inParked) {
      const t0 = performance.now();
      sel.modify("move", "forward", "character");
      walk = performance.now() - t0;
    }
    log(`parked-caret rangeCount=${sel?.rangeCount ?? "n/a"} anchorInParked=${inParked} walk=${walk.toFixed(1)}ms`);
  } else {
    await go(termIdx[0]);
  }
  await sleep(500);

  // Isolation probes: which single mutation costs a document-wide restyle?
  const layer = activeLayer();
  const parked = [...document.querySelectorAll<HTMLElement>(".layer:not(.active)")].sort(
    (a, b) => (b.textContent?.length ?? 0) - (a.textContent?.length ?? 0),
  )[0];
  const focused = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  probe("noop", () => {});
  probe("focus-blur+focus", () => {
    focused?.blur();
    force();
    focused?.focus();
  });
  probe("inert-on+off(active layer)", () => {
    layer?.setAttribute("inert", "");
    force();
    layer?.removeAttribute("inert");
  });
  probe(`inert-off+on(parked layer, ${parked?.textContent?.length ?? 0} chars)`, () => {
    parked?.removeAttribute("inert");
    force();
    parked?.setAttribute("inert", "");
  });
  probe("class-active-off+on", () => {
    layer?.classList.remove("active");
    force();
    layer?.classList.add("active");
  });
  probe("body-custom-prop", () => {
    document.body.style.setProperty("--zz-probe", "1");
    force();
    document.body.style.removeProperty("--zz-probe");
  });
  probe("style-el-insert+remove", () => {
    const st = document.createElement("style");
    st.textContent = ".zz-probe-none{color:red}";
    document.head.appendChild(st);
    force();
    st.remove();
  });
  probe("sibling-span-insert+remove", () => {
    const sp = document.createElement("span");
    layer?.parentElement?.appendChild(sp);
    force();
    sp.remove();
  });
  // Copy semantics: does a document-wide selection reach parked content?
  {
    const sel = document.getSelection();
    if (sel !== null && layer !== null) {
      sel.selectAllChildren(document.body);
      const total = sel.toString().length;
      sel.removeAllRanges();
      log(
        `select-all toString=${total} activeLayerText=${layer.textContent?.length ?? 0} bodyText=${document.body.textContent?.length ?? 0}`,
      );
      focused?.focus();
    }
  }
  log(`styles=${document.styleSheets.length} styleEls=${document.querySelectorAll("style").length}`);
  await write();

  const order = termIdx.length >= 2 ? [termIdx[0], termIdx[1]] : termIdx;
  for (let i = 0; i < 24; i++) {
    const idx = order[i % order.length];
    const dt = await go(idx);
    // The engine's own caret walk from a light-DOM caret at each end of the
    // active layer — the search WebKit's editor-state update runs per commit.
    const sel = document.getSelection();
    const focusedEl = document.activeElement;
    const cur = activeLayer();
    const last = cur === null ? null : edgeText(cur, true);
    const first = cur === null ? null : edgeText(cur, false);
    let fwd = -1;
    let back = -1;
    if (sel !== null && last !== null && first !== null) {
      const w0 = performance.now();
      sel.collapse(last, last.length);
      sel.modify("move", "forward", "character");
      fwd = performance.now() - w0;
      const w1 = performance.now();
      sel.collapse(first, 0);
      sel.modify("move", "backward", "character");
      back = performance.now() - w1;
      sel.removeAllRanges();
    }
    if (focusedEl instanceof HTMLElement) focusedEl.focus();
    log(`switch->${idx} ${dt.toFixed(1)}ms walk fwd ${fwd.toFixed(1)}ms back ${back.toFixed(1)}ms`);
    if (i % 4 === 3) void write();
    await sleep(3000);
  }
  if (fileIdx.length > 0) {
    const dt = await go(fileIdx[0]);
    log(`reveal file ${fileIdx[0]} ${dt.toFixed(1)}ms scrollTop=${scroller()?.scrollTop ?? "n/a"}`);
  }
  log("done");
  await write();
}
