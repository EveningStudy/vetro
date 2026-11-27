import type { NotificationMode, NotificationSettings, Settings, ThreadSummary } from "~/api/types";

export const DEFAULT_NOTIFICATION_SETTINGS: NotificationSettings = {
  mode: "notifications",
  onComplete: true,
  onInput: true,
  onError: true,
  backgroundOnly: true,
  badgeEnabled: true,
  badgeScope: "attention",
  badgeClear: "on_thread_open",
};

export function resolvedNotificationSettings(settings: Settings | null | undefined): NotificationSettings {
  const base = settings?.notifications;
  const mode: NotificationMode = settings?.notificationsEnabled === false && !base ? "off" : (base?.mode ?? "notifications");
  return { ...DEFAULT_NOTIFICATION_SETTINGS, ...base, mode };
}

export function hasPopup(mode: NotificationMode): boolean {
  return mode === "notifications" || mode === "notifications_and_sound";
}

export function hasSound(mode: NotificationMode): boolean {
  return mode === "sound" || mode === "notifications_and_sound";
}

export type NotificationKind = "complete" | "input" | "error";

const CHIME_NOTES: Record<NotificationKind, readonly [number, number]> = {
  complete: [523.25, 659.25],
  input: [587.33, 783.99],
  error: [392.0, 311.13],
};

let audioCtx: AudioContext | null = null;
const lastPlayedAt: Partial<Record<NotificationKind, number>> = {};

function getAudioContext(): AudioContext | null {
  if (typeof window === "undefined") return null;
  const Ctor = window.AudioContext;
  if (!Ctor) return null;
  if (!audioCtx) {
    try {
      audioCtx = new Ctor();
    } catch {
      return null;
    }
  }
  return audioCtx;
}

export function unlockNotificationAudio(): void {
  const ctx = getAudioContext();
  if (!ctx) return;
  try {
    if (ctx.state === "suspended") void ctx.resume().catch(() => undefined);
  } catch {
    // Ignore restricted autoplay environments.
  }
}

export async function playNotificationChime(kind: NotificationKind): Promise<void> {
  const ctx = getAudioContext();
  if (!ctx) return;
  const nowMs = Date.now();
  if (nowMs - (lastPlayedAt[kind] ?? 0) < 400) return;
  lastPlayedAt[kind] = nowMs;
  try {
    if (ctx.state === "suspended") await ctx.resume();
    if (ctx.state !== "running") return;
    const [first, second] = CHIME_NOTES[kind];
    const start = ctx.currentTime;
    for (const [index, freq] of [first, second].entries()) {
      const osc = ctx.createOscillator();
      const gain = ctx.createGain();
      const t0 = start + index * 0.095;
      osc.type = "sine";
      osc.frequency.setValueAtTime(freq, t0);
      gain.gain.setValueAtTime(0.0001, t0);
      gain.gain.exponentialRampToValueAtTime(0.075, t0 + 0.015);
      gain.gain.exponentialRampToValueAtTime(0.0001, t0 + 0.2);
      osc.connect(gain);
      gain.connect(ctx.destination);
      osc.start(t0);
      osc.stop(t0 + 0.21);
    }
  } catch {
    // Audio playback is best-effort.
  }
}

export function computeBadgeCount(
  threads: Record<string, ThreadSummary>,
  unreadCompleted: Record<string, boolean>,
  settings: NotificationSettings,
): number {
  if (!settings.badgeEnabled) return 0;
  const counted = new Set<string>();
  for (const thread of Object.values(threads)) {
    if (thread.parent) continue;
    if (thread.status === "awaiting_input") counted.add(thread.id);
    else if (settings.badgeScope === "attention" && unreadCompleted[thread.id]) counted.add(thread.id);
  }
  if (settings.badgeScope === "attention") {
    for (const [id, unread] of Object.entries(unreadCompleted)) {
      if (unread && !threads[id]?.parent) counted.add(id);
    }
  }
  return counted.size;
}

let originalFavicon: HTMLLinkElement | undefined;
let badgeFavicon: HTMLLinkElement | undefined;

export function setBrowserBadge(count: number): void {
  if (typeof document === "undefined") return;
  if (count <= 0) {
    if (badgeFavicon) {
      badgeFavicon.remove();
      badgeFavicon = undefined;
      if (originalFavicon) document.head.append(originalFavicon);
      originalFavicon = undefined;
    }
    return;
  }
  try {
    const canvas = document.createElement("canvas");
    canvas.width = canvas.height = 64;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.fillStyle = "#e5484d";
    ctx.beginPath();
    ctx.arc(32, 32, 28, 0, Math.PI * 2);
    ctx.fill();
    ctx.fillStyle = "#ffffff";
    ctx.font = `600 ${count > 9 ? 30 : 40}px "Segoe UI", sans-serif`;
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.fillText(count > 9 ? "9+" : String(count), 32, 34);
    if (!badgeFavicon) {
      originalFavicon = document.querySelector<HTMLLinkElement>('link[rel="icon"]') ?? undefined;
      badgeFavicon = document.createElement("link");
      badgeFavicon.rel = "icon";
      badgeFavicon.type = "image/png";
      badgeFavicon.sizes.value = "64x64";
      originalFavicon?.remove();
      document.head.append(badgeFavicon);
    }
    badgeFavicon.href = canvas.toDataURL("image/png");
  } catch {
    // Ignore headless/restricted canvas environments.
  }
}
