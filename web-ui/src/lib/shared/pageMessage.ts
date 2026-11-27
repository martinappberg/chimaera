/**
 * A page's message for an agent (the page API's `send` / `draft`, already
 * checked to follow the person's own input): one seam for every surface
 * that shows pages — HTML previews and embed cards — and one handler, which
 * the app registers (it holds the chats and the composers).
 *
 * The rule: a page in the answer of the chat it belongs to (`chat`) may
 * send to that chat; any other page — a preview pane, a card in a document,
 * a hover — only drafts into the input of the agent the person is working
 * with, who sends it or not. A page that isn't the agent's own must not be
 * able to message an agent as the person.
 */

export interface PageMessage {
  /** The chat whose transcript shows the page, when it is in one. */
  chat: string | null;
  /** The page asked to send (`send`) rather than to draft. */
  send: boolean;
  text: string;
  /** The page's own title, as it named itself. */
  title: string;
}

/** What became of a message: sent, drafted, or nowhere (no agent). */
export type PageMessageOutcome = "sent" | "drafted" | "none";

type Handler = (m: PageMessage) => PageMessageOutcome;

let handler: Handler | null = null;

export function registerPageMessages(h: Handler): () => void {
  handler = h;
  return () => {
    if (handler === h) handler = null;
  };
}

/** The message as the agent reads it: where it came from, then the text. */
export function pageMessageText(m: Pick<PageMessage, "text" | "title">): string {
  const title = m.title.trim();
  return `${title !== "" ? `From the page “${title}”:` : "From a page:"}\n\n${m.text}`;
}

export function pageMessage(m: PageMessage): PageMessageOutcome {
  return handler !== null ? handler(m) : "none";
}
