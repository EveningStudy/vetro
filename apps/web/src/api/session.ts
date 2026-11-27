const KEY = "verre:session";
const REMOTE_KEY = "verre:remote-session";
let memory: string | null = null;

export function newPairingSession(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(48));
  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 32)}.${hex.slice(32)}`;
}

export function takeLocalToken(): string | null {
  const match = /^#token=([^&]+)/.exec(location.hash);
  if (!match) return null;
  history.replaceState(null, "", `${location.pathname}${location.search}`);
  return match[1] ?? null;
}

export function getSession(): string | null {
  return readSession("sessionStorage", KEY) ??
    (location.protocol === "https:" ? readSession("localStorage", REMOTE_KEY) : null) ??
    memory;
}

export function pairedDraftStorageKey(session: string | null): string | null {
  const id = session?.match(/^([a-f0-9]{32})\.[a-f0-9]{64}$/)?.[1];
  return id ? `verre:remote-drafts:${id}` : null;
}

function readSession(storage: "sessionStorage" | "localStorage", key: string) {
  try {
    return window[storage].getItem(key);
  } catch {
    return null;
  }
}

export function setSession(session: string, rememberRemote: boolean) {
  memory = session;
  if (rememberRemote && location.protocol === "https:") {
    try {
      localStorage.setItem(REMOTE_KEY, session);
      sessionStorage.removeItem(KEY);
      return;
    } catch {
      // Use tab storage if the browser blocks persistent storage.
    }
  }
  try {
    sessionStorage.setItem(KEY, session);
  } catch {
    // The in-memory session still works with storage disabled.
  }
}

export function clearSession(session: string | null) {
  if (!session) return;
  if (memory === session) memory = null;
  removeMatchingSession("sessionStorage", KEY, session);
  if (location.protocol === "https:") removeMatchingSession("localStorage", REMOTE_KEY, session);
  const draftsKey = pairedDraftStorageKey(session);
  if (draftsKey) {
    try {
      sessionStorage.removeItem(draftsKey);
    } catch {
      return;
    }
  }
}

function removeMatchingSession(storage: "sessionStorage" | "localStorage", key: string, session: string) {
  try {
    if (window[storage].getItem(key) === session) window[storage].removeItem(key);
  } catch {
    // The browser can block storage access.
  }
}
