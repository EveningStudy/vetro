const KEY = "vetro:session";
let memory: string | null = null;

/** The session string lives in tab-scoped storage when the browser allows it, else in memory.
 * Never in localStorage and never in the URL. */
export function getSession(): string | null {
  try {
    return sessionStorage.getItem(KEY) ?? memory;
  } catch {
    return memory;
  }
}

export function setSession(session: string | null) {
  memory = session;
  try {
    if (session) sessionStorage.setItem(KEY, session);
    else sessionStorage.removeItem(KEY);
  } catch {
    // Storage can be unavailable (private mode, locked-down web views); memory is enough then.
  }
}
