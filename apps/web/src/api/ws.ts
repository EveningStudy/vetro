import type { AuthSession } from "./bindings/AuthSession";
import type { HelloParams } from "./bindings/HelloParams";
import { clearSession, getSession, setSession } from "./session";
import type { AppInfo, ServerEvent } from "./types";
import {
  type AuthApi,
  type ConnectionStatus,
  type Method,
  type Params,
  type Result,
  RpcFailure,
  type StatusListener,
  type Transport,
} from "./protocol";

type Pending = { resolve: (value: unknown) => void; reject: (error: Error) => void };

const MAX_BACKOFF_MS = 10_000;
const CONNECT_TIMEOUT_MS = 12_000;
const HEARTBEAT_INTERVAL_MS = 25_000;
const HEARTBEAT_TIMEOUT_MS = 5_000;
const AUTH_TIMEOUT_MS = 15_000;

/** One WebSocket to the core. `app.hello` goes first on every connection; requests made while
 * disconnected are queued and flushed after it. */
export class WsTransport implements Transport {
  private socket: WebSocket | null = null;
  private nextId = 1;
  private pending = new Map<number, Pending>();
  private queue: { id: number; frame: string }[] = [];
  private eventListeners = new Set<(event: ServerEvent) => void>();
  private statusListeners = new Set<StatusListener>();
  private current: ConnectionStatus = "closed";
  private attempts = 0;
  private everOpened = false;
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private checkTimer: ReturnType<typeof setTimeout> | null = null;
  private heartbeatId: number | null = null;

  constructor(
    private readonly url: string,
    private readonly hello: () => HelloParams,
  ) {}

  start() {
    this.connect();
    window.addEventListener("online", () => this.checkConnection());
    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState === "visible") this.checkConnection();
    });
  }

  status() {
    return this.current;
  }

  request<M extends Method>(method: M, params: Params<M>): Promise<Result<M>> {
    const id = this.nextId++;
    const frame = JSON.stringify({ id, method, params: params ?? {} });
    return new Promise<Result<M>>((resolve, reject) => {
      this.pending.set(id, { resolve: resolve as (value: unknown) => void, reject });
      if (this.current === "open" && this.socket) this.socket.send(frame);
      else this.queue.push({ id, frame });
    });
  }

  onEvent(listener: (event: ServerEvent) => void) {
    this.eventListeners.add(listener);
    return () => {
      this.eventListeners.delete(listener);
    };
  }

  onStatus(listener: StatusListener) {
    this.statusListeners.add(listener);
    return () => {
      this.statusListeners.delete(listener);
    };
  }

  private setStatus(status: ConnectionStatus, info: AppInfo | null = null, reconnected = false) {
    this.current = status;
    for (const listener of this.statusListeners) listener(status, { info, reconnected });
  }

  private checkConnection() {
    if (this.current === "open" && this.socket) {
      this.heartbeat(this.socket);
      return;
    }
    if (this.current !== "closed") return;
    if (this.retryTimer) clearTimeout(this.retryTimer);
    this.attempts = 0;
    this.connect();
  }

  private setCheckTimer(callback: () => void, delay: number) {
    if (this.checkTimer !== null) clearTimeout(this.checkTimer);
    this.checkTimer = setTimeout(callback, delay);
  }

  private heartbeat(socket: WebSocket) {
    if (this.socket !== socket || this.heartbeatId !== null) return;
    if (socket.readyState !== WebSocket.OPEN) {
      this.disconnect(socket);
      return;
    }
    const id = this.nextId++;
    this.heartbeatId = id;
    this.setCheckTimer(() => this.disconnect(socket), HEARTBEAT_TIMEOUT_MS);
    socket.send(JSON.stringify({ id, method: "app.ping", params: {} }));
  }

  private disconnect(socket: WebSocket) {
    if (this.socket !== socket) return;
    this.socket = null;
    this.heartbeatId = null;
    if (this.checkTimer !== null) clearTimeout(this.checkTimer);
    this.checkTimer = null;
    socket.close();
    const queued = new Set(this.queue.map((entry) => entry.id));
    for (const [id, pending] of this.pending) {
      if (queued.has(id)) continue;
      pending.reject(new RpcFailure("disconnected", "Connection lost"));
      this.pending.delete(id);
    }
    const delay = Math.min(MAX_BACKOFF_MS, 500 * 2 ** this.attempts++) * (0.75 + Math.random() / 2);
    this.retryTimer = setTimeout(() => this.connect(), delay);
    this.setStatus("closed");
  }

  private connect() {
    this.retryTimer = null;
    this.setStatus("connecting");
    const socket = new WebSocket(this.url);
    this.socket = socket;
    this.setCheckTimer(() => this.disconnect(socket), CONNECT_TIMEOUT_MS);
    socket.onopen = () => {
      if (this.socket !== socket) return;
      const id = this.nextId++;
      this.pending.set(id, {
        resolve: (info) => {
          this.attempts = 0;
          const reconnected = this.everOpened;
          this.everOpened = true;
          const queued = this.queue;
          this.queue = [];
          this.current = "open";
          this.setCheckTimer(() => this.heartbeat(socket), HEARTBEAT_INTERVAL_MS);
          for (const entry of queued) socket.send(entry.frame);
          this.setStatus("open", info as AppInfo, reconnected);
        },
        reject: () => this.disconnect(socket),
      });
      socket.send(JSON.stringify({ id, method: "app.hello", params: this.hello() }));
    };
    socket.onmessage = (message) => {
      if (this.socket === socket) this.handle(String(message.data), socket);
    };
    socket.onclose = () => this.disconnect(socket);
  }

  private handle(text: string, socket: WebSocket) {
    let frame: {
      id?: number;
      result?: unknown;
      error?: { code: string; message: string };
      event?: string;
    };
    try {
      frame = JSON.parse(text);
    } catch {
      return;
    }
    if (typeof frame.id === "number") {
      if (frame.id === this.heartbeatId) {
        this.heartbeatId = null;
        this.setCheckTimer(() => this.heartbeat(socket), HEARTBEAT_INTERVAL_MS);
        return;
      }
      const pending = this.pending.get(frame.id);
      if (!pending) return;
      this.pending.delete(frame.id);
      if (frame.error) pending.reject(new RpcFailure(frame.error.code, frame.error.message));
      else pending.resolve(frame.result ?? {});
      return;
    }
    if (frame.event === "app.resync") {
      if (this.current === "open") this.setStatus("open", null, true);
      return;
    }
    if (typeof frame.event === "string") {
      for (const listener of this.eventListeners) listener(frame as unknown as ServerEvent);
    }
  }
}

export function wsUrl() {
  const scheme = location.protocol === "https:" ? "wss:" : "ws:";
  return `${scheme}//${location.host}/api/ws`;
}

async function authRequest<T>(path: string, init: RequestInit, read: (response: Response) => Promise<T>) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), AUTH_TIMEOUT_MS);
  try {
    const response = await fetch(path, { ...init, credentials: "same-origin", signal: controller.signal });
    return await read(response);
  } catch (error) {
    if (controller.signal.aborted) throw new DOMException("Authentication request timed out", "TimeoutError");
    throw error;
  } finally {
    clearTimeout(timer);
  }
}

function postAuth<T>(path: string, body: unknown, read: (response: Response) => Promise<T>) {
  return authRequest(path, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  }, read);
}

async function optionalJson<T>(response: Response): Promise<T | null> {
  try {
    return await response.json() as T;
  } catch (error) {
    if (error instanceof SyntaxError) return null;
    throw error;
  }
}

async function keepSession(response: Response, rememberRemote = false) {
  const body = await optionalJson<Partial<AuthSession>>(response);
  if (body && typeof body.session === "string") setSession(body.session, rememberRemote && body.remoteClient === true);
  return body;
}

export const httpAuth: AuthApi = {
  async session() {
    const session = getSession();
    return authRequest("/api/auth/session", {
      headers: session ? { authorization: `Bearer ${session}` } : {},
    }, async (response) => {
      if (response.status === 401) {
        clearSession(session);
        return null;
      }
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      return response.json();
    });
  },
  local(token) {
    return postAuth("/api/auth/local", { token }, async (response) => {
      if (response.status === 429 || response.status >= 500) throw new Error(`HTTP ${response.status}`);
      if (!response.ok) return { ok: false };
      const body = await keepSession(response);
      return { ok: true, remoteClient: body?.remoteClient };
    });
  },
  pair(code, deviceName, proposedSession, remember) {
    return postAuth("/api/auth/pair", { code, deviceName, proposedSession }, async (response) => {
      if (response.ok) {
        const body = await keepSession(response, remember);
        return { ok: typeof body?.session === "string" };
      }
      const body = await optionalJson<{ error?: { message?: string }; message?: string }>(response);
      return { ok: false, status: response.status, message: body?.error?.message ?? body?.message };
    });
  },
};
