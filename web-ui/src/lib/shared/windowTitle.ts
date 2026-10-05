/**
 * The window / tab title, composed from what the window shows. Pure so the
 * wording is pinned by tests; App.svelte feeds it and sets `document.title`.
 *
 *   "my-analysis •hpc | chimaera"        a remote window, in a workspace
 *   "my-analysis | chimaera"                  local — the host is implicit
 *   "my-analysis •In the cloud | chimaera"    a browser view of a project
 *   "claude (2) — my-analysis | chimaera"     a detached solo window: tab leads
 */

/** What a window wears after the workspace name, or null when it wears none.
 *  A project view follows its project between the cloud and the user's
 *  computer, so it names where the project runs (the status strip's own
 *  label) — never a host alias, and nothing at all until the first placement
 *  read says where (a generic "This project" placeholder would only read as
 *  a machine's name). Any other window wears its host when it is remote. */
export function titleHost(o: {
  projectView: boolean;
  /** The strip's "In the cloud" / "On your computer" label; null until known. */
  projectLabel: string | null;
  hostAlias: string;
  remote: boolean;
}): string | null {
  if (o.projectView) return o.projectLabel;
  return o.remote ? o.hostAlias : null;
}

export function windowTitle(o: {
  workspace: string | null;
  host: string | null;
  /** A compute-node daemon's node: "hpc › node-044", so a job window
   *  never poses as its login node. */
  node?: string | null;
  /** A detached solo window is named for what it shows: the tab leads. */
  tab?: string | null;
  needsYou: number;
}): string {
  const host = o.host !== null && o.node ? `${o.host} › ${o.node}` : o.host;
  let scope = o.workspace !== null ? (host !== null ? `${o.workspace} •${host}` : o.workspace) : (host ?? "");
  if (o.tab) scope = scope ? `${o.tab} — ${scope}` : o.tab;
  const base = scope ? `${scope} | chimaera` : "chimaera";
  return o.needsYou > 0 ? `(${o.needsYou}) ${base}` : base;
}
