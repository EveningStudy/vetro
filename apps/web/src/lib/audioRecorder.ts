const TARGET_SAMPLE_RATE = 16_000;
const PREROLL_CHUNKS = 3;
const MIN_SPEECH_MS = 450;
const TRAILING_SILENCE_MS = 650;
const MAX_SEGMENT_MS = 12_000;
const TAIL_KEEP_CHUNKS = 2;

export interface AudioSegment {
  base64: string;
  durationMs: number;
}

export interface RecordingSession {
  stop: () => Promise<AudioSegment>;
  cancel: () => void;
}

/** Captures microphone audio and encodes it as 16 kHz mono 16-bit PCM WAV,
 * optionally emitting pause-segmented phrases while recording. */
export async function startRecording(
  signal: AbortSignal,
  onLevel?: (level: number) => void,
  onSegment?: (segment: AudioSegment) => void,
): Promise<RecordingSession> {
  if (!navigator.mediaDevices?.getUserMedia) {
    throw new Error("Microphone recording is not available in this browser.");
  }
  const stream = await navigator.mediaDevices.getUserMedia({
    audio: {
      channelCount: 1,
      echoCancellation: true,
      noiseSuppression: true,
      autoGainControl: true,
    },
  });

  const AudioCtx = window.AudioContext ?? (window as unknown as { webkitAudioContext: typeof AudioContext }).webkitAudioContext;
  let ctx: AudioContext | undefined;
  try {
    if (signal.aborted) throw new DOMException("Recording cancelled", "AbortError");
    ctx = new AudioCtx();
    return connectRecorder(stream, ctx, onLevel, onSegment);
  } catch (error) {
    for (const track of stream.getTracks()) track.stop();
    void ctx?.close().catch(() => undefined);
    throw error;
  }
}

function connectRecorder(
  stream: MediaStream,
  ctx: AudioContext,
  onLevel?: (level: number) => void,
  onSegment?: (segment: AudioSegment) => void,
): RecordingSession {
  const source = ctx.createMediaStreamSource(stream);
  const processor = ctx.createScriptProcessor(4096, 1, 1);
  const silentGain = ctx.createGain();
  silentGain.gain.value = 0;

  const tailChunks: Float32Array[] = [];
  let tailSamples = 0;
  let tailPeakRms = 0;
  let preroll: Float32Array[] = [];
  let segmentChunks: Float32Array[] = [];
  let segmentSamples = 0;
  let speechSamples = 0;
  let silenceSamples = 0;
  let speaking = false;
  let emittedSegments = 0;
  let noiseFloor = 0.008;
  let closed = false;
  const startedAt = performance.now();

  const clearTail = () => {
    tailChunks.length = 0;
    tailSamples = 0;
    tailPeakRms = 0;
  };

  const encodeChunks = (chunks: Float32Array[], totalSamples: number, sampleRate: number): AudioSegment => {
    const merged = new Float32Array(totalSamples);
    let offset = 0;
    for (const chunk of chunks) {
      merged.set(chunk, offset);
      offset += chunk.length;
    }
    const resampled = downsample(merged, sampleRate, TARGET_SAMPLE_RATE);
    const wav = encodeWav(resampled, TARGET_SAMPLE_RATE);
    const durationMs = Math.round((totalSamples / Math.max(1, sampleRate)) * 1000);
    return { base64: bytesToBase64(wav), durationMs };
  };

  const resetSegment = () => {
    segmentChunks = [];
    segmentSamples = 0;
    speechSamples = 0;
    silenceSamples = 0;
    speaking = false;
  };

  const flushVadSegment = (sampleRate: number) => {
    if (!onSegment || segmentSamples === 0) return;
    // Trim excessive trailing silence while keeping ~170ms tail for clean word endings.
    const extraSilentChunks = Math.max(0, Math.floor((silenceSamples / Math.max(1, segmentChunks[0]?.length ?? 4096)) - TAIL_KEEP_CHUNKS));
    const usableCount = Math.max(1, segmentChunks.length - extraSilentChunks);
    const slice = segmentChunks.slice(0, usableCount);
    const samples = slice.reduce((sum, chunk) => sum + chunk.length, 0);
    resetSegment();
    preroll = [];
    if (samples / Math.max(1, sampleRate) < 0.25) return;
    emittedSegments += 1;
    clearTail();
    onSegment(encodeChunks(slice, samples, sampleRate));
  };

  processor.onaudioprocess = (event) => {
    if (closed) return;
    const sampleRate = ctx.sampleRate || 48_000;
    const input = event.inputBuffer.getChannelData(0);
    const copy = new Float32Array(input.length);
    copy.set(input);

    let sum = 0;
    for (let index = 0; index < copy.length; index += 1) sum += copy[index]! * copy[index]!;
    const rms = Math.sqrt(sum / Math.max(1, copy.length));
    onLevel?.(Math.min(1, rms * 4.5));

    tailChunks.push(copy);
    tailSamples += copy.length;
    tailPeakRms = Math.max(tailPeakRms, rms);
    const maxTailSamples = sampleRate * (onSegment ? 45 : 120);
    while (tailSamples > maxTailSamples && tailChunks.length > 1) {
      tailSamples -= tailChunks.shift()!.length;
    }
    if (!onSegment) return;

    const onsetThreshold = Math.max(0.015, noiseFloor * 2.5);
    const continueThreshold = Math.max(0.010, noiseFloor * 1.7);
    const isSpeech = speaking ? rms > continueThreshold : rms > onsetThreshold;

    if (!speaking) {
      if (isSpeech) {
        speaking = true;
        for (const chunk of preroll) {
          segmentChunks.push(chunk);
          segmentSamples += chunk.length;
        }
        preroll = [];
        segmentChunks.push(copy);
        segmentSamples += copy.length;
        speechSamples += copy.length;
        silenceSamples = 0;
      } else {
        noiseFloor = Math.min(0.035, Math.max(0.004, noiseFloor * 0.96 + rms * 0.04));
        preroll.push(copy);
        if (preroll.length > PREROLL_CHUNKS) preroll.shift();
      }
      return;
    }

    segmentChunks.push(copy);
    segmentSamples += copy.length;
    if (isSpeech) {
      speechSamples += copy.length;
      silenceSamples = 0;
    } else {
      silenceSamples += copy.length;
    }

    const speechMs = (speechSamples / sampleRate) * 1000;
    const silenceMs = (silenceSamples / sampleRate) * 1000;
    const totalMs = (segmentSamples / sampleRate) * 1000;
    if ((silenceMs >= TRAILING_SILENCE_MS && speechMs >= MIN_SPEECH_MS) || totalMs >= MAX_SEGMENT_MS) {
      flushVadSegment(sampleRate);
    }
  };

  source.connect(processor);
  processor.connect(silentGain);
  silentGain.connect(ctx.destination);

  const cleanup = () => {
    if (closed) return;
    closed = true;
    processor.onaudioprocess = null;
    try {
      source.disconnect();
      processor.disconnect();
      silentGain.disconnect();
    } catch {
      // Ignore already disconnected nodes.
    }
    for (const track of stream.getTracks()) track.stop();
    void ctx.close().catch(() => undefined);
  };

  return {
    cancel: cleanup,
    stop: async () => {
      const sampleRate = ctx.sampleRate || 48_000;
      const wallDurationMs = Math.max(0, Math.round(performance.now() - startedAt));
      cleanup();
      if (!onSegment) {
        if (tailSamples === 0) return { base64: "", durationMs: 0 };
        const encoded = encodeChunks(tailChunks, tailSamples, sampleRate);
        return { base64: encoded.base64, durationMs: wallDurationMs };
      }
      if (speaking && speechSamples > 0) {
        const segment = encodeChunks(segmentChunks, segmentSamples, sampleRate);
        if (segment.durationMs >= 250) return segment;
      }
      const tailDurationMs = (tailSamples / Math.max(1, sampleRate)) * 1000;
      const hasAudibleTail = tailDurationMs >= 250 && tailPeakRms > Math.max(0.006, noiseFloor * 1.3);
      if (tailSamples > 0 && ((emittedSegments === 0 && wallDurationMs >= 250) || hasAudibleTail)) {
        const encoded = encodeChunks(tailChunks, tailSamples, sampleRate);
        return {
          base64: encoded.base64,
          durationMs: emittedSegments === 0 ? wallDurationMs : encoded.durationMs,
        };
      }
      return { base64: "", durationMs: 0 };
    },
  };
}

function downsample(samples: Float32Array, fromRate: number, toRate: number): Float32Array {
  if (samples.length === 0 || fromRate <= toRate) return samples;
  const ratio = fromRate / toRate;
  const length = Math.max(1, Math.round(samples.length / ratio));
  const out = new Float32Array(length);
  for (let index = 0; index < length; index += 1) {
    const start = Math.floor(index * ratio);
    const end = Math.min(samples.length, Math.floor((index + 1) * ratio));
    let sum = 0;
    let count = 0;
    for (let pos = start; pos < end; pos += 1) {
      sum += samples[pos]!;
      count += 1;
    }
    out[index] = count > 0 ? sum / count : 0;
  }
  return out;
}

function encodeWav(samples: Float32Array, sampleRate: number): Uint8Array {
  const dataLength = samples.length * 2;
  const buffer = new ArrayBuffer(44 + dataLength);
  const view = new DataView(buffer);
  writeAscii(view, 0, "RIFF");
  view.setUint32(4, 36 + dataLength, true);
  writeAscii(view, 8, "WAVE");
  writeAscii(view, 12, "fmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, 1, true);
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, sampleRate * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  writeAscii(view, 36, "data");
  view.setUint32(40, dataLength, true);
  let offset = 44;
  for (let index = 0; index < samples.length; index += 1) {
    const clamped = Math.max(-1, Math.min(1, samples[index]!));
    view.setInt16(offset, clamped < 0 ? clamped * 0x8000 : clamped * 0x7fff, true);
    offset += 2;
  }
  return new Uint8Array(buffer);
}

function writeAscii(view: DataView, offset: number, text: string) {
  for (let index = 0; index < text.length; index += 1) view.setUint8(offset + index, text.charCodeAt(index));
}

function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  const step = 0x8000;
  for (let offset = 0; offset < bytes.length; offset += step) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + step));
  }
  return btoa(binary);
}
