/**
 * Microphone → 16 kHz mono little-endian PCM16, the format Claude's speech
 * service takes (`encoding=linear16&sample_rate=16000&channels=1`).
 *
 * The context runs at the device's own rate (a 16 kHz context would make the
 * browser resample, which Firefox refuses across contexts); an AudioWorklet
 * averages each output sample's span of input — a box filter, enough low-pass
 * for speech — and posts ~100 ms chunks with their RMS level for the meter.
 * The mic is held only while recording: `stop()` releases the device.
 */

/** Output rate and chunk length (samples) — 100 ms. */
const TARGET_RATE = 16000;
const CHUNK_SAMPLES = 1600;

const PROCESSOR = "chimaera-pcm16";

// Inlined as a Blob URL: a worklet must load from its own module URL, and a
// string keeps it out of the bundler's asset graph.
const WORKLET_SOURCE = `
class Pcm16 extends AudioWorkletProcessor {
  constructor() {
    super();
    this.ratio = sampleRate / ${TARGET_RATE};
    this.pos = 0;
    this.acc = 0;
    this.accN = 0;
    this.out = new Int16Array(${CHUNK_SAMPLES});
    this.n = 0;
    this.sumSq = 0;
    this.port.onmessage = (e) => {
      if (e.data === "flush") {
        this.emit();
        this.port.postMessage({ flushed: true });
      }
    };
  }
  emit() {
    if (this.n === 0) return;
    const chunk = this.out.slice(0, this.n);
    const rms = Math.sqrt(this.sumSq / this.n);
    this.n = 0;
    this.sumSq = 0;
    this.port.postMessage({ pcm: chunk.buffer, rms }, [chunk.buffer]);
  }
  process(inputs) {
    const input = inputs[0];
    if (!input || input.length === 0) return true;
    const channels = input.length;
    const frames = input[0].length;
    for (let i = 0; i < frames; i++) {
      let v = 0;
      for (let c = 0; c < channels; c++) v += input[c][i];
      this.acc += v / channels;
      this.accN++;
      this.pos += 1;
      if (this.pos >= this.ratio) {
        this.pos -= this.ratio;
        let s = this.acc / this.accN;
        this.acc = 0;
        this.accN = 0;
        s = s < -1 ? -1 : s > 1 ? 1 : s;
        this.out[this.n++] = s < 0 ? s * 0x8000 : s * 0x7fff;
        this.sumSq += s * s;
        if (this.n === ${CHUNK_SAMPLES}) this.emit();
      }
    }
    return true;
  }
}
registerProcessor("${PROCESSOR}", Pcm16);
`;

let workletUrl: string | null = null;

function moduleUrl(): string {
  workletUrl ??= URL.createObjectURL(new Blob([WORKLET_SOURCE], { type: "text/javascript" }));
  return workletUrl;
}

export interface Capture {
  /** The input actually recording, as the system names it. */
  device: string;
  /** Emit the partial last chunk, then release the microphone. */
  stop(): Promise<void>;
}

export interface Microphone {
  id: string;
  label: string;
}

/** The pseudo-inputs Chromium lists beside the real ones ("default", the
 *  Windows "communications" alias) — duplicates of a real device. */
const ALIAS_IDS = new Set(["default", "communications"]);

async function audioInputs(): Promise<Microphone[]> {
  const devices = await navigator.mediaDevices.enumerateDevices();
  return devices
    .filter((d) => d.kind === "audioinput" && !ALIAS_IDS.has(d.deviceId))
    .map((d) => ({ id: d.deviceId, label: d.label }));
}

/**
 * This machine's microphones, by name. Names are withheld until the page
 * has been granted the microphone once, so an unnamed list asks first.
 * Chosen inputs are remembered by NAME: device ids are per origin, and a
 * daemon's origin (its port) changes between runs.
 */
export async function listMicrophones(): Promise<Microphone[]> {
  if (!microphoneSupported()) throw insecure();
  let inputs = await audioInputs();
  if (inputs.length > 0 && inputs.every((d) => d.label === "")) {
    await checkMicrophone();
    inputs = await audioInputs();
  }
  return inputs.filter((d) => d.label !== "");
}

/** A capture failure in words for the composer. */
export class CaptureError extends Error {
  constructor(
    message: string,
    readonly code: "insecure" | "denied" | "no_device" | "failed",
  ) {
    super(message);
    this.name = "CaptureError";
  }
}

/** Whether this page can reach a microphone at all (secure context + API). */
export function microphoneSupported(): boolean {
  return typeof navigator !== "undefined" && typeof navigator.mediaDevices?.getUserMedia === "function";
}

function captureError(e: unknown): CaptureError {
  const name = e instanceof DOMException ? e.name : "";
  if (name === "NotAllowedError" || name === "SecurityError") {
    return new CaptureError(
      "Microphone access is off — allow it in System Settings → Privacy & Security → Microphone.",
      "denied",
    );
  }
  if (name === "NotFoundError" || name === "OverconstrainedError") {
    return new CaptureError("No microphone found.", "no_device");
  }
  if (name === "NotReadableError") {
    return new CaptureError("Couldn't open the microphone — another app may be using it.", "failed");
  }
  return new CaptureError(`Couldn't start the microphone (${String(e)}).`, "failed");
}

function insecure(): CaptureError {
  return new CaptureError(
    "Browsers allow the microphone only over https or localhost.",
    "insecure",
  );
}

/** Ask for the microphone once, without recording: the permission prompt
 *  belongs to turning voice on, not to the first word spoken. */
export async function checkMicrophone(): Promise<void> {
  if (!microphoneSupported()) throw insecure();
  let stream: MediaStream;
  try {
    stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  } catch (e) {
    throw captureError(e);
  }
  for (const track of stream.getTracks()) track.stop();
}

/**
 * Record from `microphone` (a name from `listMicrophones`; empty or no longer
 * present = the system default input).
 */
export async function startCapture(
  onChunk: (pcm: ArrayBuffer, level: number) => void,
  microphone = "",
): Promise<Capture> {
  if (!microphoneSupported()) throw insecure();
  let deviceId: string | undefined;
  if (microphone !== "") {
    deviceId = (await audioInputs().catch(() => [])).find((d) => d.label === microphone)?.id;
  }
  let stream: MediaStream;
  try {
    stream = await navigator.mediaDevices.getUserMedia({
      audio: {
        ...(deviceId !== undefined ? { deviceId: { exact: deviceId } } : {}),
        channelCount: 1,
        echoCancellation: true,
        noiseSuppression: true,
        autoGainControl: true,
      },
    });
  } catch (e) {
    throw captureError(e);
  }
  const device = stream.getAudioTracks()[0]?.label || "the default microphone";
  let context: AudioContext | null = null;
  try {
    context = new AudioContext();
    await context.audioWorklet.addModule(moduleUrl());
    if (context.state === "suspended") await context.resume();
    const source = context.createMediaStreamSource(stream);
    const node = new AudioWorkletNode(context, PROCESSOR, {
      numberOfInputs: 1,
      numberOfOutputs: 1,
      outputChannelCount: [1],
    });
    let flushed: (() => void) | null = null;
    node.port.onmessage = (e: MessageEvent) => {
      const data = e.data as { pcm?: ArrayBuffer; rms?: number; flushed?: boolean };
      if (data.pcm !== undefined) onChunk(data.pcm, data.rms ?? 0);
      if (data.flushed) flushed?.();
    };
    source.connect(node);
    // Pulled by the destination so the worklet runs; it writes no output, so
    // this is silence — the user never hears themselves.
    node.connect(context.destination);
    const ctx = context;
    let stopped = false;
    return {
      device,
      async stop() {
        if (stopped) return;
        stopped = true;
        await new Promise<void>((resolve) => {
          const timer = setTimeout(resolve, 250);
          flushed = () => {
            clearTimeout(timer);
            resolve();
          };
          node.port.postMessage("flush");
        });
        source.disconnect();
        node.disconnect();
        node.port.onmessage = null;
        for (const track of stream.getTracks()) track.stop();
        await ctx.close().catch(() => {});
      },
    };
  } catch (e) {
    for (const track of stream.getTracks()) track.stop();
    await context?.close().catch(() => {});
    throw e instanceof CaptureError ? e : captureError(e);
  }
}
