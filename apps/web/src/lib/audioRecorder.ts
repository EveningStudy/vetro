const TARGET_SAMPLE_RATE = 16_000;

export interface RecordingSession {
  stop: () => Promise<{ base64: string; durationMs: number }>;
  cancel: () => void;
}

/** Captures microphone audio and encodes it as 16 kHz mono 16-bit PCM WAV. */
export async function startRecording(onLevel?: (level: number) => void): Promise<RecordingSession> {
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
  const ctx = new AudioCtx();
  const source = ctx.createMediaStreamSource(stream);
  const processor = ctx.createScriptProcessor(4096, 1, 1);
  const silentGain = ctx.createGain();
  silentGain.gain.value = 0;

  const chunks: Float32Array[] = [];
  let totalSamples = 0;
  let closed = false;
  const startedAt = performance.now();

  processor.onaudioprocess = (event) => {
    if (closed) return;
    const input = event.inputBuffer.getChannelData(0);
    const copy = new Float32Array(input.length);
    copy.set(input);
    chunks.push(copy);
    totalSamples += copy.length;
    if (onLevel) {
      let sum = 0;
      for (let index = 0; index < copy.length; index += 1) sum += copy[index]! * copy[index]!;
      const rms = Math.sqrt(sum / Math.max(1, copy.length));
      onLevel(Math.min(1, rms * 4.5));
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
      const durationMs = Math.max(0, Math.round(performance.now() - startedAt));
      cleanup();
      const merged = new Float32Array(totalSamples);
      let offset = 0;
      for (const chunk of chunks) {
        merged.set(chunk, offset);
        offset += chunk.length;
      }
      const resampled = downsample(merged, sampleRate, TARGET_SAMPLE_RATE);
      const wav = encodeWav(resampled, TARGET_SAMPLE_RATE);
      return { base64: bytesToBase64(wav), durationMs };
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
