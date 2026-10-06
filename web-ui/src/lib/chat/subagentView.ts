/**
 * The read-only view of one subagent's own conversation.
 *
 * A subagent is one row in its parent chat; "opening" it shows what it did,
 * as a chat of its own. That view is ChatView over an ordinary ChatStore,
 * fed by `SubagentSocket` (polling `GET /sessions/{id}/subagents/{agent}/
 * transcript`) instead of a chat socket. It is keyed in the chat pool by a
 * synthetic id built here, so nothing can mistake it for a session.
 */
import type { BackgroundTask, ChatBlock, SubagentInfo } from "./store.svelte";

/** Which subagent of which chat. */
export interface SubagentRef {
  /** The chat session that spawned it. */
  parentId: string;
  /** The agent's own key for the subagent (`SubagentInfo.agentId`). */
  agentId: string;
}

const PREFIX = "sub:";

/** The chat-pool key of a subagent view. Session ids never contain a colon
 *  and never start with `sub:`, so the two id spaces cannot collide. */
export function subagentChatId(ref: SubagentRef): string {
  return `${PREFIX}${ref.parentId}:${ref.agentId}`;
}

/** The inverse of {@link subagentChatId}; null for a session id. */
export function parseSubagentChatId(id: string): SubagentRef | null {
  if (!id.startsWith(PREFIX)) return null;
  const rest = id.slice(PREFIX.length);
  const cut = rest.lastIndexOf(":");
  if (cut <= 0 || cut === rest.length - 1) return null;
  return { parentId: rest.slice(0, cut), agentId: rest.slice(cut + 1) };
}

/** A subagent's name from its tool row: the drivers title these
 *  "Agent: {description}", and the prefix is the surface's own label. */
export function subagentTitle(rowTitle: string): string {
  for (const prefix of ["Agent: ", "Task: "]) {
    if (rowTitle.startsWith(prefix)) return rowTitle.slice(prefix.length);
  }
  return rowTitle;
}

/** What the parent chat currently knows about its subagents. */
export interface SubagentSource {
  backgroundTasks: readonly BackgroundTask[];
  activeAgents: readonly Extract<ChatBlock, { kind: "tool" }>[];
  subagents: ReadonlyMap<string, SubagentInfo>;
}

/** Whether the parent still has `agentId` working: a lane in its background
 *  set, or an in-flight Agent row bound to it. */
export function subagentRunning(parent: SubagentSource, agentId: string): boolean {
  if (parent.backgroundTasks.some((task) => task.id === agentId)) return true;
  return parent.activeAgents.some((row) => parent.subagents.get(row.id)?.agentId === agentId);
}

/** The model serving `agentId`, from whichever surface named it. */
export function subagentModel(parent: SubagentSource, agentId: string): string | null {
  for (const info of parent.subagents.values()) {
    if (info.agentId === agentId && info.model !== null) return info.model;
  }
  return parent.backgroundTasks.find((task) => task.id === agentId)?.model ?? null;
}

/** One transcript answer (the daemon's `subagents.rs`). */
export interface SubagentRead {
  agent: string;
  /** Names the window `events` are numbered within. */
  epoch: string | null;
  /** Index of `events[0]` within the epoch. */
  from: number;
  events: { type: string; [key: string]: unknown }[];
  /** When each event happened (epoch ms, 0 = not known), parallel to
   *  `events`; absent when the source keeps no times. */
  ts?: number[];
  model?: string | null;
  /** Opaque; sent back so an unchanged source answers without a read. */
  stamp?: string | null;
}

/** What a reader holds of one subagent's conversation. */
export interface SubagentCursor {
  epoch: string | null;
  /** Events applied within `epoch`. */
  held: number;
  stamp: string | null;
}

export const EMPTY_CURSOR: SubagentCursor = { epoch: null, held: 0, stamp: null };

/**
 * Fold one answer into the reader's cursor. `restart` means the window
 * moved (or this is the first answer): the reader drops what it rendered
 * and applies `events` from the top. Otherwise `events` are exactly the
 * ones after the reader's `held` — an answer that starts anywhere else was
 * made for a different cursor (a slow response overtaken by a newer one)
 * and is ignored.
 */
export function advanceCursor(
  cursor: SubagentCursor,
  read: SubagentRead,
): {
  cursor: SubagentCursor;
  restart: boolean;
  events: SubagentRead["events"];
  timestamps: number[];
} | null {
  const sameWindow = cursor.epoch !== null && read.epoch === cursor.epoch;
  if (sameWindow && read.from !== cursor.held) return null;
  if (!sameWindow && read.from !== 0) return null;
  return {
    cursor: {
      epoch: read.epoch,
      held: (sameWindow ? cursor.held : 0) + read.events.length,
      stamp: read.stamp ?? null,
    },
    restart: !sameWindow,
    events: read.events,
    timestamps: read.ts ?? [],
  };
}
