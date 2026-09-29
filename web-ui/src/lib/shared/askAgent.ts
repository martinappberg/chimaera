/**
 * "Ask an agent": draft a request into the composer of the agent the user
 * is working with — never send it. One function for every surface that
 * offers it (Knowledge's Tidy up, plugin screens' `ask-agent` action). The
 * app registers the handler (it knows the layout and the sessions); a file
 * and lines, when given, ride along as a reference the agent can open.
 */

export interface AskRequest {
  text: string;
  /** Workspace-relative or absolute. */
  file?: string;
  line?: number;
  end_line?: number;
}

/** Drafts the request; the agent's display name, or null when there is no
 *  live agent to ask in this workspace. */
type Handler = (text: string) => string | null;

let handler: Handler | null = null;

export function registerAskAgent(h: Handler): () => void {
  handler = h;
  return () => {
    if (handler === h) handler = null;
  };
}

/** The draft's text: the request, then its place as `file:line-end`. */
export function askText(req: AskRequest): string {
  if (req.file === undefined || req.file === "") return req.text;
  const lines =
    req.line !== undefined && req.line > 0
      ? `:${req.line}${req.end_line !== undefined && req.end_line > req.line ? `-${req.end_line}` : ""}`
      : "";
  return `${req.text}\n\n(${req.file}${lines})`;
}

export function askAgent(req: AskRequest): string | null {
  return handler !== null ? handler(askText(req)) : null;
}
