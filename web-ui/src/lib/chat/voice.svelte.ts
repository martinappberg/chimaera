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
      if (!hostCheck.available) recheckOnReturn();
    });
  return hostCheckInFlight;
}

let recheckArmed = false;

/** No dictation here (yet): ask again when the user comes back to this
 *  window — after signing in elsewhere, or once a restarting daemon is back.
 *  One armed listener pair; nothing runs while the window stays away. */
function recheckOnReturn(): void {
  if (recheckArmed || typeof window === "undefined") return;
  recheckArmed = true;
  const again = () => {
    if (document.visibilityState !== "visible") return;
    window.removeEventListener("focus", again);
    document.removeEventListener("visibilitychange", again);
    recheckArmed = false;
    void recheckHost();
  };
  window.addEventListener("focus", again);
  document.addEventListener("visibilitychange", again);
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

/** Bars in the waveform beside the stop button. */
const WAVE_BARS = 5;
/** Audio held client-side until a phrase's socket opens: ~30 s at 100 ms. */
const MAX_QUEUED_CHUNKS = 300;
/** How long `finish()` waits for the daemon's `done` before settling for
 *  what it has (the daemon's own finalize wait is 5 s). */
const FINISH_TIMEOUT_MS = 8000;
/** Chunks kept between phrases, so a phrase that opens on the first sound
 *  still gets the syllable before it: ~300 ms. */
const PREROLL_CHUNKS = 3;
/** Loudest 100 ms (raw RMS) under which a recording was silence: a working
 *  mic in a quiet room still reads ~0.001–0.01; a muted one (a closed
 *  laptop's built-in mic) reads ~0. */
const SILENT_RMS = 0.0005;
/** The relay's error codes that mean the login, not the recording, failed. */
const LOGIN_CODES = new Set(["no_login", "expired", "unreadable", "auth"]);

/**
 * Where one spoken phrase ends: enough speech, then a real pause. Fed one
 * RMS level per ~100 ms chunk. "Silent" is relative to how loud the speaker
 * has been lately (noise suppression and AGC vary by mic), with a floor.
 */
export class PauseDetector {
  static readonly CHUNK_MS = 100;
  static readonly MIN_SPEECH_MS = 600;
  static readonly PAUSE_MS = 800;
  /** A phrase this long takes a shorter breath as its end, so unbroken
   *  talking still gets corrected every so often. */
  static readonly LONG_PHRASE_MS = 10_000;
  static readonly SHORT_PAUSE_MS = 400;
  /** Whether the last chunk fed was silence. */
  silent = true;
  private recent: number[] = [];
  private speechMs = 0;
  private silenceMs = 0;

  /** True when this chunk completes a pause after a phrase. */
  feed(rms: number): boolean {
    this.recent.push(rms);
    if (this.recent.length > 30) this.recent.shift();
    const loudest = Math.max(...this.recent);
    const silent = rms < Math.max(0.008, loudest * 0.12);
    this.silent = silent;
    if (!silent) {
      this.speechMs += PauseDetector.CHUNK_MS;
      this.silenceMs = 0;
      return false;
    }
    this.silenceMs += PauseDetector.CHUNK_MS;
    const pause =
      this.speechMs >= PauseDetector.LONG_PHRASE_MS ? PauseDetector.SHORT_PAUSE_MS : PauseDetector.PAUSE_MS;
    if (this.speechMs >= PauseDetector.MIN_SPEECH_MS && this.silenceMs >= pause) {
      this.speechMs = 0;
      this.silenceMs = 0;
      return true;
    }
    return false;
  }
}

/**
 * One spoken phrase: its own `/ws/voice` stream. The speech service revises
 * nothing while audio flows — a wrong early guess (the wrong language, say)
 * stands until the stream is finalized — so a recording finalizes each phrase
 * at the pause after it and speaks on into a fresh one: every phrase is
 * corrected a moment after it's said, and each fresh stream picks its
 * language anew.
 */
class Phrase {
  finals = "";
  interim = "";
  /** Finalize sent: no more audio; the corrected text is on its way. */
  closing = false;
  done = false;
  private ws: WebSocket;
  private opened = false;
  private queue: ArrayBuffer[] = [];
  private waiters: (() => void)[] = [];

  constructor(
    keyterms: string[],
    private readonly onChange: () => void,
    private readonly onError: (message: string, code: string | undefined) => void,
  ) {
    const proto = location.protocol === "https:" ? "wss" : "ws";
    const ws = new WebSocket(`${proto}://${location.host}/ws/voice`);
    ws.binaryType = "arraybuffer";
    this.ws = ws;
    ws.onopen = () => {
      ws.send(JSON.stringify({ type: "auth", token: getToken() ?? "" }));
      ws.send(JSON.stringify({ type: "start", language: dictationLanguage().code, keyterms }));
      this.opened = true;
      for (const chunk of this.queue) ws.send(chunk);
      this.queue = [];
      if (this.closing) ws.send(JSON.stringify({ type: "finalize" }));
    };
    ws.onmessage = (ev: MessageEvent) => {
      if (typeof ev.data !== "string") return;
      let msg: { type?: string; text?: string; message?: string; code?: string };
      try {
        msg = JSON.parse(ev.data) as typeof msg;
      } catch {
        return;
      }
      switch (msg.type) {
        case "interim":
          this.interim = msg.text ?? "";
          this.onChange();
          break;
        case "final":
          this.finals = joinSpoken(this.finals, msg.text ?? "");
          this.interim = "";
          this.onChange();
          break;
        case "error":
          this.onError(msg.message ?? "Dictation failed.", msg.code);
          break;
        case "done":
          this.finish();
          break;
      }
    };
    ws.onclose = () => this.finish();
  }

  get text(): string {
    return joinSpoken(this.finals, this.interim);
  }

  send(pcm: ArrayBuffer): void {
    if (this.closing) return;
    if (this.opened && this.ws.readyState === WebSocket.OPEN) this.ws.send(pcm);
    else if (this.queue.length < MAX_QUEUED_CHUNKS) this.queue.push(pcm);
  }

  /** No more audio: ask for the corrected text (sent on open if needed). */
  finalize(): void {
    if (this.closing) return;
    this.closing = true;
    if (this.opened && this.ws.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify({ type: "finalize" }));
    }
  }

  whenDone(): Promise<void> {
    return this.done ? Promise.resolve() : new Promise((resolve) => this.waiters.push(resolve));
  }

  cancel(): void {
    if (this.opened && this.ws.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify({ type: "cancel" }));
    }
    this.close();
  }

  close(): void {
    this.ws.onclose = null;
    this.ws.onmessage = null;
    if (this.ws.readyState !== WebSocket.CLOSED) this.ws.close();
    this.finish(false);
  }

  private finish(notify = true): void {
    if (this.done) return;
    this.done = true;
    if (this.ws.readyState !== WebSocket.CLOSED) this.ws.close();
    for (const resolve of this.waiters.splice(0)) resolve();
    if (notify) this.onChange();
  }
}

export class Dictation {
  state = $state<DictationState>("idle");
  /** The last few 0..1 input levels (one per ~100 ms), oldest first. */
  levels = $state<number[]>(new Array(WAVE_BARS).fill(0));
  /** Text the service has corrected: the finished phrases, joined. */
  finals = $state("");
  /** Text still forming: the phrases after those. */
  interim = $state("");
  /** The last failure, for the composer to show. */
  error = $state<string | null>(null);
  /** The input recording now, as the system names it. */
  device = $state("");

  /** This recording's phrases in order; the last is the one being spoken
   *  unless it is closing (then the next opens when speech resumes). */
  private phrases: Phrase[] = [];
  /** Audio since the last phrase closed, until the next one opens. */
  private preroll: ArrayBuffer[] = [];
  private keyterms: string[] = [];
  private pauses = new PauseDetector();
  private capture: Capture | null = null;
  /** Bumps per recording, so a late event from an old one can't touch this. */
  private generation = 0;
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
    this.keyterms = opts.keyterms;
    this.pauses = new PauseDetector();
    this.preroll = [];
    this.phrases = [this.phrase(generation)];
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
        this.error = e instanceof CaptureError ? e.message : String(e);
        this.end();
      }
      return false;
    }
  }

  private phrase(generation: number): Phrase {
    const phrase: Phrase = new Phrase(
      this.keyterms,
      () => {
        if (generation !== this.generation) return;
        this.refresh();
        // The phrase being spoken ended by itself (the relay failed, a
        // cap): the recording ends with what was heard.
        if (phrase.done && !phrase.closing && this.state !== "finishing") {
          this.error ??= "Dictation stopped — lost the connection to chimaera.";
          this.end();
        }
      },
      (message, code) => {
        if (generation !== this.generation) return;
        this.error = message;
        if (code !== undefined && LOGIN_CODES.has(code)) void recheckHost();
      },
    );
    return phrase;
  }

  /** Finished phrases lead as corrected text; the first unfinished one and
   *  everything after it is still forming. */
  private refresh(): void {
    let settled = "";
    let live = "";
    let leading = true;
    for (const p of this.phrases) {
      if (leading && p.done) settled = joinSpoken(settled, p.text);
      else {
        leading = false;
        live = joinSpoken(live, p.text);
      }
    }
    this.finals = settled;
    this.interim = live;
  }

  private onAudio(generation: number, pcm: ArrayBuffer, level: number): void {
    if (generation !== this.generation) return;
    this.peak = Math.max(this.peak, level);
    // Speech RMS sits well under 0.3; a square-root curve lifts quiet
    // talking into view without pinning loud speech at the top.
    this.levels = [...this.levels.slice(1), Math.min(1, Math.sqrt(level * 6))];
    const pause = this.pauses.feed(level);
    const current = this.phrases.at(-1);
    if (current === undefined || current.closing) {
      // Between phrases a stream opens only once speech resumes — a
      // recording that ends in silence opens none — and takes the last few
      // chunks along so its first syllable isn't clipped.
      this.preroll.push(pcm);
      if (this.preroll.length > PREROLL_CHUNKS) this.preroll.shift();
      if (this.pauses.silent || this.state !== "listening") return;
      const next = this.phrase(generation);
      this.phrases.push(next);
      for (const chunk of this.preroll.splice(0)) next.send(chunk);
      return;
    }
    current.send(pcm);
    if (pause && this.state === "listening" && current.text !== "") current.finalize();
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
    this.phrases.at(-1)?.finalize();
    await Promise.race([
      Promise.all(this.phrases.map((p) => p.whenDone())),
      new Promise((resolve) => setTimeout(resolve, FINISH_TIMEOUT_MS)),
    ]);
    if (generation !== this.generation) return null;
    const text = joinSpoken(...this.phrases.map((p) => p.text));
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
    for (const p of this.phrases) p.cancel();
    this.end();
  }

  /** Back to idle: release the mic and the sockets, stale events fenced off. */
  private end(): void {
    this.generation++;
    const capture = this.capture;
    this.capture = null;
    void capture?.stop();
    for (const p of this.phrases) p.close();
    this.phrases = [];
    this.preroll = [];
    this.state = "idle";
    this.interim = "";
  }
}
