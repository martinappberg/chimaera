/**
 * Voice dictation for the chat composer — its mic button (and `/voice on|off`),
 * the same for Claude and Codex chats. The words come from Claude's speech-to-text
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

/** The draft while dictating: the user's own text around the spoken words,
 *  which are split into what has settled and what is still forming. */
export interface DictationParts {
  /** The text before the words, with the space that separates them. */
  before: string;
  finals: string;
  /** The space between settled and forming words. */
  gap: string;
  interim: string;
  /** The space after the words, then the text that followed the caret. */
  after: string;
}

/** Lay out live dictation between `before` and `after` (the draft split at
 *  the caret), spaced like typed words. `joinParts` is the draft itself. */
export function dictationParts(
  before: string,
  after: string,
  finals: string,
  interim: string,
): DictationParts {
  const f = finals.trim();
  const i = interim.trim();
  const spoken = f.length > 0 || i.length > 0;
  const lead = spoken && before.length > 0 && !/\s$/.test(before) ? " " : "";
  const trail = spoken && after.length > 0 && !/^\s/.test(after) ? " " : "";
  return { before: before + lead, finals: f, gap: f && i ? " " : "", interim: i, after: trail + after };
}

export function joinParts(p: DictationParts): string {
  return p.before + p.finals + p.gap + p.interim + p.after;
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

/** Whether the daemon's host can dictate (it has the claude.ai login the
 *  speech service needs), asked once per window and again after a
 *  recording fails on the login — so an always-on mic hides itself where
 *  dictation can't work, and comes back after a sign-in. */
let hostCheck = $state<{ asked: boolean; available: boolean }>({ asked: false, available: false });
let hostCheckInFlight: Promise<void> | null = null;

export function hostCanDictate(): boolean {
  if (!hostCheck.asked) void recheckHost();
  return hostCheck.available;
}

export function recheckHost(): Promise<void> {
  hostCheckInFlight ??= api("/voice", { signal: AbortSignal.timeout(8000) })
    .then(async (res) => {
      const body = res.ok ? ((await res.json()) as { available?: boolean }) : {};
      hostCheck = { asked: true, available: body.available === true };
    })
    .catch(() => {
      hostCheck = { asked: true, available: false };
    })
    .finally(() => {
      hostCheckInFlight = null;
    });
  return hostCheckInFlight;
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
    if (res.status === 404) return "This host's chimaera is too old for dictation — update it.";
    if (!res.ok) return `Couldn't check dictation (HTTP ${res.status}).`;
    const body = (await res.json()) as { available?: boolean; reason?: string };
    hostCheck = { asked: true, available: body.available === true };
    if (body.available !== true) return body.reason ?? "Dictation isn't available on this host.";
  } catch (e) {
    return `Couldn't check dictation: ${String(e)}`;
  }
  try {
    await checkMicrophone();
  } catch (e) {
    return e instanceof CaptureError ? e.message : String(e);
  }
  return null;
}

export type DictationState = "idle" | "starting" | "listening" | "finishing";

/** Bars in the listening strip's waveform. */
const WAVE_BARS = 5;
/** Audio held client-side until the relay socket opens: ~30 s at 100 ms. */
const MAX_QUEUED_CHUNKS = 300;
/** How long `finish()` waits for the daemon's `done` before settling for
 *  what it has (the daemon's own finalize wait is 5 s). */
const FINISH_TIMEOUT_MS = 8000;
/** Loudest 100 ms (raw RMS) under which a recording was silence: a working
 *  mic in a quiet room still reads ~0.001–0.01; a muted one (a closed
 *  laptop's built-in mic) reads ~0. */
const SILENT_RMS = 0.0005;
/** The relay's error codes that mean the login, not the recording, failed. */
const LOGIN_CODES = new Set(["no_login", "expired", "unreadable", "auth"]);

export class Dictation {
  state = $state<DictationState>("idle");
  /** The last few 0..1 input levels (one per ~100 ms), oldest first. */
  levels = $state<number[]>(new Array(WAVE_BARS).fill(0));
  /** Settled utterances, joined. */
  finals = $state("");
  /** The utterance being heard now. */
  interim = $state("");
  /** The last failure, for the composer to show. */
  error = $state<string | null>(null);
  /** The input recording now, as the system names it. */
  device = $state("");

  private ws: WebSocket | null = null;
  private opened = false;
  private queue: ArrayBuffer[] = [];
  private capture: Capture | null = null;
  /** Bumps per recording, so a late event from an old one can't touch this. */
  private generation = 0;
  /** Resolves a pending `finish()` (done, closed, or cancelled). */
  private settle: (() => void) | null = null;
  /** Loudest chunk this recording (raw RMS). */
  private peak = 0;

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
    this.levels = new Array(WAVE_BARS).fill(0);
    this.peak = 0;
    this.device = "";
    this.queue = [];
    this.opened = false;
    this.open(generation, opts.keyterms);
    try {
      const capture = await startCapture(
        (pcm, level) => this.onAudio(generation, pcm, level),
        getSetting("chat.voiceMicrophone"),
      );
      if (generation !== this.generation) {
        void capture.stop();
        return false;
      }
      this.capture = capture;
      this.device = capture.device;
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
      let msg: { type?: string; text?: string; message?: string; code?: string };
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
          this.error = msg.message ?? "Dictation failed.";
          if (msg.code !== undefined && LOGIN_CODES.has(msg.code)) void recheckHost();
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
        this.error ??= "Dictation stopped — lost the connection to chimaera.";
        this.end();
      }
    };
  }

  private onAudio(generation: number, pcm: ArrayBuffer, level: number): void {
    if (generation !== this.generation) return;
    this.peak = Math.max(this.peak, level);
    // Speech RMS sits well under 0.3; a square-root curve lifts quiet
    // talking into view without pinning loud speech at the top.
    this.levels = [...this.levels.slice(1), Math.min(1, Math.sqrt(level * 6))];
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
    // The last words settle here, so what's shown never flickers at the end.
    this.finals = text;
    this.interim = "";
    if (text === "" && this.error === null) {
      this.error =
        this.peak < SILENT_RMS && this.device !== ""
          ? `No sound from ${this.device} — right-click the mic to switch.`
          : "Didn't catch that.";
    }
    this.end();
    return text;
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
    this.interim = "";
  }
}
