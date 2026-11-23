import type { RawEntry } from "~/api/types";

const LISTING = /list$/i;
const HANDSHAKE = /"method"\s*:\s*"(initialize|initialized|authenticate)"/;

type Frame = { id?: unknown; method?: unknown };

function frameOf(text: string): Frame | null {
  try {
    const value = JSON.parse(text);
    return value && typeof value === "object" ? (value as Frame) : null;
  } catch {
    return null;
  }
}

/** One agent process can serve many threads (Codex app-server, ACP agents), so its wire log
 * mixes them. Keep the process start and handshake, lines that name this session, and the
 * request/response partners of those lines. */
export function linesForThread(entries: RawEntry[], nativeId: string): Set<number> {
  const keep = new Set<number>();
  const frames = entries.map((entry) => (entry.direction === "sent" || entry.direction === "received" ? frameOf(entry.text) : null));
  const clientRequests = new Map<string, number>();
  const serverRequests = new Map<string, number>();
  const pendingServerIds = new Set<string>();
  const wantedClientIds = new Set<string>();
  const listing = new Set<string>();
  entries.forEach((entry, index) => {
    const frame = frames[index];
    if (entry.direction === "sent" && frame && typeof frame.method === "string" && LISTING.test(frame.method) && frame.id != null)
      listing.add(String(frame.id));
  });

  entries.forEach((entry, index) => {
    const frame = frames[index];
    const id = frame && frame.id !== undefined && frame.id !== null ? String(frame.id) : null;
    const isRequest = !!frame && typeof frame.method === "string";
    if (entry.direction === "spawn" || HANDSHAKE.test(entry.text)) keep.add(index);
    if (id && !isRequest && entry.direction === "received" && listing.has(id)) return;
    if (id && isRequest && entry.direction === "sent") clientRequests.set(id, index);
    if (id && isRequest && entry.direction === "received") serverRequests.set(id, index);
    if (isRequest && typeof frame?.method === "string" && LISTING.test(frame.method)) return;
    const mentions = entry.text.includes(nativeId);
    if (!mentions) return;
    keep.add(index);
    if (!id) return;
    if (isRequest && entry.direction === "received") pendingServerIds.add(id);
    if (!isRequest && entry.direction === "received") wantedClientIds.add(id);
    if (isRequest && entry.direction === "sent") wantedClientIds.add(id);
  });

  entries.forEach((entry, index) => {
    const frame = frames[index];
    if (!frame || frame.id === undefined || frame.id === null) return;
    const id = String(frame.id);
    const isRequest = typeof frame.method === "string";
    if (!isRequest && entry.direction === "sent" && pendingServerIds.has(id)) keep.add(index);
    if (!isRequest && entry.direction === "received" && wantedClientIds.has(id)) keep.add(index);
    if (isRequest && entry.direction === "sent" && wantedClientIds.has(id)) keep.add(index);
  });
  for (const id of wantedClientIds) {
    const index = clientRequests.get(id);
    if (index !== undefined) keep.add(index);
  }
  for (const id of pendingServerIds) {
    const index = serverRequests.get(id);
    if (index !== undefined) keep.add(index);
  }
  return keep;
}
