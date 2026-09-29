/**
 * Voice dictation for the chat composer — Chimaera's `/voice`, the same for
 * Claude and Codex chats. The words come from Claude's speech-to-text
 * service (the one Claude Code's own `/voice` uses), reached through the
 * daemon's `/ws/voice` relay with the claude.ai login on the daemon's host;
 * the microphone is this window's (`voiceCapture.ts`).
 *
 * One `Dictation` per composer, one socket per recording: `start()` opens the
 * relay and the mic together (audio captured before the relay is up is held,
 * never dropped), transcripts stream in as `finals` + `interim`, `finish()`
 * resolves with the whole text, `cancel()` discards it.
 */

import { api, getToken } from "../net/api";
import { getSetting } from "../settings/store.svelte";
import { checkMicrophone, startCapture, type Capture, CaptureError } from "./voiceCapture";
import { DICTATION_LANGUAGES } from "./voiceLanguages";

/** The `/voice` modes, spelled as Claude Code spells them. */
export type VoiceMode = "hold" | "tap";


/** The language a recording asks for: the setting, or this browser's own
 *  when it is one the service takes — else English, as claude falls back. */
export function dictationLanguage(): { code: string; name: string } {
  const chosen = getSetting("chat.voiceLanguage");
  const wanted = chosen === "auto" ? (navigator.language ?? "en") : chosen;
  const base = wanted.toLowerCase().split(/[-_]/)[0];
  return DICTATION_LANGUAGES.find((l) => l.code === base) ?? DICTATION_LANGUAGES[0];
}

/** Join utterances the way they were spoken: one space between. */
export function joinSpoken(...parts: string[]): string {
  return parts
    .map((p) => p.trim())
    .filter((p) => p.length > 0)
    .join(" ");
}

/**
 * Put dictated `text` into `draft` at `at`, spaced from its neighbors so it
 * reads as typed words. Returns the new draft and the caret after the text.
 */
export function insertDictation(
  draft: string,
  at: number,
  text: string,
): { draft: string; caret: number } {
  const pos = Math.max(0, Math.min(at, draft.length));
  const before = draft.slice(0, pos);
  const after = draft.slice(pos);
  const lead = before.length > 0 && !/\s$/.test(before) ? " " : "";
  const trail = after.length > 0 && !/^\s/.test(after) ? " " : "";
  const inserted = `${lead}${text}${trail}`;
  return { draft: before + inserted + after, caret: pos + lead.length + text.length };
}

/**
 * Can this window dictate right now? Resolves with the reason it can't, in
 * words for a notice — null when it can. Asks for the microphone (so the
 * permission prompt comes with turning voice on) and whether the daemon's
 * host has the claude.ai login the speech service needs.
 */
export async function voiceProblem(): Promise<string | null> {
  try {
    const res = await api("/voice", { signal: AbortSignal.timeout(8000) });
    if (res.status === 404) return "This daemon is too old for voice dictation — update chimaera on this host.";
    if (!res.ok) return `Couldn't check voice dictation (HTTP ${res.status}).`;
    const body = (await res.json()) as { available?: boolean; reason?: string };
    if (body.available !== true) return body.reason ?? "Voice dictation isn't available on this host.";
  } catch (e) {
    return `Couldn't check voice dictation: ${String(e)}`;
  }
  try {
    await checkMicrophone();
  } catch (e) {
    return e instanceof CaptureError ? e.message : String(e);
  }
  return null;
}

export type DictationState = "idle" | "starting" | "listening" | "finishing";

/** Audio held client-side until the relay socket opens: ~30 s at 100 ms. */
const MAX_QUEUED_CHUNKS = 300;
/** How long `finish()` waits for the daemon's `done` before settling for
 *  what it has (the daemon's own finalize wait is 5 s). */
const FINISH_TIMEOUT_MS = 8000;

export class Dictation {
  state = $state<DictationState>("idle");
  /** 0..1 input level for the meter, per ~100 ms. */
  level = $state(0);
  /** Settled utterances, joined. */
  finals = $state("");
  /** The utterance being heard now. */
  interim = $state("");
  /** The last failure, for the composer to show. */
  error = $state<string | null>(null);

  private ws: WebSocket | null = null;
  private opened = false;
  private queue: ArrayBuffer[] = [];
  private capture: Capture | null = null;
  /** Bumps per recording, so a late event from an old one can't touch this. */
  private generation = 0;
  /** Resolves a pending `finish()` (done, closed, or cancelled). */
  private settle: (() => void) | null = null;

  get active(): boolean {
    return this.state !== "idle";
  }

  /** Start a recording. False when it couldn't (see `error`). */
  async start(opts: { keyterms: string[] }): Promise<boolean> {
    if (this.state !== "idle") return false;
    const generation = ++this.generation;
    this.state = "starting";
    this.error = null;
    this.finals = "";
    this.interim = "";
    this.level = 0;
    this.queue = [];
    this.opened = false;
    this.open(generation, opts.keyterms);
    try {
      const capture = await startCapture((pcm, level) => this.onAudio(generation, pcm, level));
      if (generation !== this.generation) {
        void capture.stop();
        return false;
      }
      this.capture = capture;
      if (this.state === "starting") this.state = "listening";
      return true;
    } catch (e) {
      if (generation === this.generation) {
        this.fail(e instanceof CaptureError ? e.message : String(e));
      }
      return false;
    }
  }

  private open(generation: number, keyterms: string[]): void {
    const proto = location.protocol === "https:" ? "wss" : "ws";
    const ws = new WebSocket(`${proto}://${location.host}/ws/voice`);
    ws.binaryType = "arraybuffer";
    this.ws = ws;
    ws.onopen = () => {
      if (generation !== this.generation) return;
      ws.send(JSON.stringify({ type: "auth", token: getToken() ?? "" }));
      ws.send(
        JSON.stringify({ type: "start", language: dictationLanguage().code, keyterms }),
      );
      this.opened = true;
      for (const chunk of this.queue) ws.send(chunk);
      this.queue = [];
    };
    ws.onmessage = (ev: MessageEvent) => {
      if (generation !== this.generation || typeof ev.data !== "string") return;
      let msg: { type?: string; text?: string; message?: string };
      try {
        msg = JSON.parse(ev.data) as typeof msg;
      } catch {
        return;
      }
      switch (msg.type) {
        case "interim":
          this.interim = msg.text ?? "";
          break;
        case "final":
          this.finals = joinSpoken(this.finals, msg.text ?? "");
          this.interim = "";
          break;
        case "error":
          this.error = msg.message ?? "voice dictation failed";
          break;
        case "done":
          this.settle?.();
          if (this.state !== "finishing") this.end();
          break;
      }
    };
    ws.onclose = () => {
      if (generation !== this.generation) return;
      this.ws = null;
      if (this.settle !== null) {
        this.settle();
      } else if (this.state !== "idle") {
        // The relay ended the recording itself (an error, the length cap).
        this.error ??= "voice dictation stopped — the connection to chimaera closed";
        this.end();
      }
    };
  }

  private onAudio(generation: number, pcm: ArrayBuffer, level: number): void {
    if (generation !== this.generation) return;
    // RMS of speech sits well under 0.3; stretch it so the meter moves.
    this.level = Math.min(1, level * 4);
    const ws = this.ws;
    if (ws !== null && this.opened && ws.readyState === WebSocket.OPEN) {
      ws.send(pcm);
    } else if (this.queue.length < MAX_QUEUED_CHUNKS) {
      this.queue.push(pcm);
    }
  }

  /**
   * Stop recording and wait for the last words. Resolves with everything
   * heard (possibly empty), or null when there was no recording to finish.
   */
  async finish(): Promise<string | null> {
    if (this.state === "idle" || this.state === "finishing") return null;
    const generation = this.generation;
    this.state = "finishing";
    this.level = 0;
    const capture = this.capture;
    this.capture = null;
    await capture?.stop();
    if (generation !== this.generation) return null;
    const ws = this.ws;
    if (ws !== null && ws.readyState !== WebSocket.CLOSED) {
      await new Promise<void>((resolve) => {
        const timer = setTimeout(() => settle(), FINISH_TIMEOUT_MS);
        const settle = () => {
          clearTimeout(timer);
          this.settle = null;
          resolve();
        };
        this.settle = settle;
        const send = () => ws.send(JSON.stringify({ type: "finalize" }));
        if (ws.readyState === WebSocket.OPEN && this.opened) {
          for (const chunk of this.queue) ws.send(chunk);
          this.queue = [];
          send();
        } else {
          // Still connecting: `onopen` flushes the queue; finalize after it.
          ws.addEventListener("open", () => queueMicrotask(send), { once: true });
        }
      });
    }
    if (generation !== this.generation) return null;
    const text = joinSpoken(this.finals, this.interim);
    this.end();
    return text;
  }

  /** Show a composer-side outcome (nothing heard) where failures show. */
  report(message: string): void {
    this.error = message;
  }

  clearError(): void {
    this.error = null;
  }

  /** Stop and discard. */
  cancel(): void {
    if (this.state === "idle") return;
    const ws = this.ws;
    if (ws !== null && ws.readyState === WebSocket.OPEN && this.opened) {
      ws.send(JSON.stringify({ type: "cancel" }));
    }
    this.settle?.();
    this.end();
  }

  private fail(message: string): void {
    this.error = message;
    this.end();
  }

  /** Back to idle: release the mic and the socket, stale events fenced off. */
  private end(): void {
    this.generation++;
    const capture = this.capture;
    this.capture = null;
    void capture?.stop();
    const ws = this.ws;
    this.ws = null;
    if (ws !== null && ws.readyState !== WebSocket.CLOSED) ws.close();
    this.queue = [];
    this.opened = false;
    this.settle = null;
    this.state = "idle";
    this.level = 0;
    this.interim = "";
  }
}
