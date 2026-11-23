import type { HelloParams } from "./bindings/HelloParams";
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

  constructor(
    private readonly url: string,
    private readonly hello: HelloParams,
  ) {}

  start() {
    this.connect();
    window.addEventListener("online", () => this.reconnectNow());
    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState === "visible") this.reconnectNow();
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

  private reconnectNow() {
    if (this.current !== "closed") return;
    if (this.retryTimer) clearTimeout(this.retryTimer);
    this.attempts = 0;
    this.connect();
  }

  private connect() {
    this.retryTimer = null;
    this.setStatus("connecting");
    const socket = new WebSocket(this.url);
    this.socket = socket;
    socket.onopen = () => {
      const id = this.nextId++;
      this.pending.set(id, {
        resolve: (info) => {
          this.attempts = 0;
          const reconnected = this.everOpened;
          this.everOpened = true;
          const queued = this.queue;
          this.queue = [];
          this.current = "open";
          for (const entry of queued) socket.send(entry.frame);
          this.setStatus("open", info as AppInfo, reconnected);
        },
        reject: () => socket.close(),
      });
      socket.send(JSON.stringify({ id, method: "app.hello", params: this.hello }));
    };
    socket.onmessage = (message) => this.handle(String(message.data));
    socket.onclose = () => {
      if (this.socket !== socket) return;
      this.socket = null;
      const queued = new Set(this.queue.map((entry) => entry.id));
      for (const [id, pending] of this.pending) {
        if (queued.has(id)) continue;
        pending.reject(new RpcFailure("disconnected", "Connection lost"));
        this.pending.delete(id);
      }
      this.setStatus("closed");
      const delay = Math.min(MAX_BACKOFF_MS, 500 * 2 ** this.attempts++) * (0.75 + Math.random() / 2);
      this.retryTimer = setTimeout(() => this.connect(), delay);
    };
  }

  private handle(text: string) {
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

function postJson(path: string, body: unknown) {
  return fetch(path, {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}

export const httpAuth: AuthApi = {
  async session() {
    const response = await fetch("/api/auth/session", { credentials: "same-origin" });
    if (response.status === 401) return null;
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    return response.json();
  },
  async local(token) {
    const response = await postJson("/api/auth/local", { token });
    return response.ok;
  },
  async pair(code, deviceName) {
    const response = await postJson("/api/auth/pair", { code, deviceName });
    if (response.ok) return { ok: true };
    const body = await response.json().catch(() => null);
    return { ok: false, message: body?.error?.message ?? body?.message };
  },
};
