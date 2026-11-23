// Builds the mock UI, serves it on loopback and captures screenshots with headless Edge.
// Edge is driven over the DevTools protocol so the viewport is exact (plain --window-size has a
// minimum width) and a shot can type or click first. Output: <repo>/.orchestra/shots/*.png.
// Usage: pnpm shots [name-filter]
import { execFileSync, spawn } from "node:child_process";
import { createReadStream, existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { extname, join, normalize, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const webDir = resolve(fileURLToPath(new URL("..", import.meta.url)));
const repoDir = resolve(webDir, "../..");
const outDir = join(repoDir, ".orchestra", "shots");
const distDir = join(webDir, "dist-mock");
const profileDir = join(repoDir, ".orchestra", "tmp", "edge-profile");
const edge = process.env.EDGE_PATH ?? "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe";
const filter = process.argv[2];

const id = (value) => encodeURIComponent(value);
const MAIN = id("codex:019a7f3e-raw-log");
const PENDING = id("devin:lovely-quartz-4821");
const QUESTION = id("claude:9a8b7c6d-rate-limit");
const PARENT = id("claude:6c1d2e4f-acp-review");
const LONG = id("codex:0199e0aa-index-hardening");
const READONLY = id("antigravity:conv-3f9e-landing");

const desktop = { width: 1440, height: 900, scale: 1, mobile: false };
const phone = { width: 390, height: 844, scale: 2, mobile: true };

const send = (text, waitMs) => [
  { type: "focus", selector: "textarea" },
  { type: "text", text },
  { type: "key", key: "Enter" },
  { type: "wait", ms: waitMs },
];

const shots = [
  ["thread-dark", `/t/${MAIN}?theme=dark`, desktop],
  ["thread-light", `/t/${MAIN}?theme=light`, desktop],
  ["streaming", `/t/${MAIN}?theme=dark`, desktop, send("Check the working tree, then build the web app.", 1100)],
  ["streaming-permission", `/t/${MAIN}?theme=light`, desktop, send("Check the working tree, then build the web app.", 9000)],
  ["stream-complete", `/t/${MAIN}?theme=dark`, desktop, [...send("Check the working tree, then build the web app.", 9000), { type: "click", text: "Allow once" }, { type: "wait", ms: 2500 }]],
  ["new-thread-started", `/new?theme=light`, desktop, send("Reply with one word.", 1500)],
  ["new-thread-dark", `/new?theme=dark`, desktop],
  ["new-thread-light", `/new?theme=light&agent=antigravity`, desktop],
  ["mention-popup", `/new?theme=dark`, desktop, [{ type: "focus", selector: "textarea" }, { type: "text", text: "Explain @comp" }, { type: "wait", ms: 600 }]],
  ["settings-agents", `/settings/agents?theme=dark`, desktop],
  ["settings-custom-agent", `/settings/agents?theme=light`, desktop, [{ type: "click", text: "Add agent" }, { type: "wait", ms: 400 }, { type: "focus", selector: "#custom-name" }, { type: "text", text: "Echo ACP" }, { type: "wait", ms: 300 }]],
  ["signin-device-code", `/new?theme=dark&agent=antigravity`, desktop, [{ type: "click", text: "Sign in with a code" }, { type: "wait", ms: 1500 }]],
  ["remote-client-settings", `/settings/remote?theme=dark&remote=1`, desktop],
  ["settings-remote", `/settings/remote?theme=light&pairing=1`, desktop],
  ["settings-about-zh", `/settings/about?theme=dark&lang=zh`, desktop],
  ["permission-request", `/t/${PENDING}?theme=dark`, desktop],
  ["question-request", `/t/${QUESTION}?theme=light`, desktop],
  ["child-session-expanded", `/t/${PARENT}?theme=dark&expand=children`, desktop],
  ["raw-log-panel", `/t/${MAIN}?theme=dark&panel=raw`, desktop],
  ["long-thread", `/t/${LONG}?theme=dark`, desktop],
  ["read-only", `/t/${READONLY}?theme=light`, desktop],
  ["palette", `/t/${MAIN}?theme=dark&palette=1`, desktop, [{ type: "text", text: "raw" }, { type: "wait", ms: 600 }]],
  ["empty", `/?theme=light`, desktop],
  ["thread-zh", `/t/${PARENT}?theme=light&lang=zh`, desktop],
  ["pairing", `/?theme=dark&pair=1&code=K7M-4QX`, phone],
  ["phone-thread", `/t/${MAIN}?theme=dark`, phone],
  ["phone-light", `/t/${PENDING}?theme=light`, phone],
  ["phone-sidebar", `/t/${MAIN}?theme=dark&sidebar=1`, phone],
  ["phone-new-thread", `/new?theme=light`, phone],
  ["phone-raw-log", `/t/${MAIN}?theme=dark&panel=raw`, phone],
  ["phone-settings", `/settings/remote?theme=dark&pairing=1`, phone],
].filter(([name]) => !filter || name.includes(filter));

if (!existsSync(edge)) {
  console.error(`Edge not found at ${edge}; set EDGE_PATH.`);
  process.exit(1);
}

console.log("building mock UI…");
execFileSync(process.execPath, [join(webDir, "node_modules", "vite", "bin", "vite.js"), "build", "--mode", "mock", "--outDir", "dist-mock", "--logLevel", "warn"], {
  cwd: webDir,
  stdio: "inherit",
});

const types = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript",
  ".css": "text/css",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".woff2": "font/woff2",
  ".json": "application/json",
};

const server = createServer((req, res) => {
  const url = new URL(req.url ?? "/", "http://127.0.0.1");
  let file = normalize(join(distDir, decodeURIComponent(url.pathname)));
  if (!file.startsWith(distDir) || !existsSync(file) || statSync(file).isDirectory()) file = join(distDir, "index.html");
  res.writeHead(200, { "content-type": types[extname(file)] ?? "application/octet-stream", "cache-control": "no-store" });
  createReadStream(file).pipe(res);
});
await new Promise((done) => server.listen(0, "127.0.0.1", done));
const { port } = server.address();

rmSync(profileDir, { recursive: true, force: true });
mkdirSync(profileDir, { recursive: true });
mkdirSync(outDir, { recursive: true });

const browser = spawn(
  edge,
  [
    "--headless=new",
    "--disable-gpu",
    "--hide-scrollbars",
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-extensions",
    "--disable-sync",
    "--remote-debugging-port=0",
    `--user-data-dir=${profileDir}`,
    "about:blank",
  ],
  { stdio: "ignore" },
);

const portFile = join(profileDir, "DevToolsActivePort");
const deadline = Date.now() + 30_000;
while (!existsSync(portFile) || readFileSync(portFile, "utf8").split("\n").length < 2) {
  if (Date.now() > deadline) throw new Error("Edge did not open a DevTools port");
  await new Promise((r) => setTimeout(r, 100));
}
const [debugPort, browserPath] = readFileSync(portFile, "utf8").trim().split("\n");

const socket = new WebSocket(`ws://127.0.0.1:${debugPort}${browserPath}`);
await new Promise((done, fail) => {
  socket.onopen = done;
  socket.onerror = fail;
});
let nextId = 1;
const pending = new Map();
const listeners = new Set();
socket.onmessage = (message) => {
  const data = JSON.parse(message.data);
  if (data.id && pending.has(data.id)) {
    const { done, fail } = pending.get(data.id);
    pending.delete(data.id);
    if (data.error) fail(new Error(data.error.message));
    else done(data.result);
  } else for (const listener of listeners) listener(data);
};
const cdp = (method, params = {}, sessionId) =>
  new Promise((done, fail) => {
    const callId = nextId++;
    pending.set(callId, { done, fail });
    socket.send(JSON.stringify({ id: callId, method, params, ...(sessionId ? { sessionId } : {}) }));
  });
const once = (method, sessionId) =>
  new Promise((done) => {
    const listener = (event) => {
      if (event.method === method && event.sessionId === sessionId) {
        listeners.delete(listener);
        done(event.params);
      }
    };
    listeners.add(listener);
  });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function capture(name, path, view, actions = []) {
  const lang = path.includes("lang=") ? "" : "&lang=en";
  const url = `http://127.0.0.1:${port}${path}${path.includes("?") ? "&" : "?"}mock=1${lang}`;
  const { targetId } = await cdp("Target.createTarget", { url: "about:blank" });
  const { sessionId } = await cdp("Target.attachToTarget", { targetId, flatten: true });
  try {
    await cdp("Page.enable", {}, sessionId);
    await cdp("Emulation.setDeviceMetricsOverride", { width: view.width, height: view.height, deviceScaleFactor: view.scale, mobile: view.mobile }, sessionId);
    if (view.mobile) await cdp("Emulation.setTouchEmulationEnabled", { enabled: true, maxTouchPoints: 5 }, sessionId);
    const loaded = once("Page.loadEventFired", sessionId);
    await cdp("Page.navigate", { url }, sessionId);
    await loaded;
    await sleep(1200);
    for (const action of actions) {
      if (action.type === "wait") await sleep(action.ms);
      if (action.type === "focus")
        await cdp("Runtime.evaluate", { expression: `document.querySelector(${JSON.stringify(action.selector)})?.focus()` }, sessionId);
      if (action.type === "click")
        await cdp("Runtime.evaluate", { expression: `[...document.querySelectorAll("button")].find((b) => b.textContent.trim() === ${JSON.stringify(action.text)})?.click()` }, sessionId);
      if (action.type === "text") await cdp("Input.insertText", { text: action.text }, sessionId);
      if (action.type === "key") {
        const key = { key: action.key, code: action.key, windowsVirtualKeyCode: action.key === "Enter" ? 13 : 0 };
        await cdp("Input.dispatchKeyEvent", { type: "keyDown", ...key }, sessionId);
        await cdp("Input.dispatchKeyEvent", { type: "keyUp", ...key }, sessionId);
      }
    }
    await sleep(300);
    const { data } = await cdp("Page.captureScreenshot", { format: "png" }, sessionId);
    const file = join(outDir, `${name}.png`);
    writeFileSync(file, Buffer.from(data, "base64"));
    console.log(`✓ ${name} (${view.width}x${view.height}@${view.scale}x) ${file}`);
  } catch (error) {
    console.log(`✗ ${name}: ${error.message}`);
  } finally {
    await cdp("Target.closeTarget", { targetId }).catch(() => undefined);
  }
}

try {
  for (const [name, path, view, actions] of shots) await capture(name, path, view, actions);
} finally {
  await cdp("Browser.close").catch(() => undefined);
  socket.close();
  browser.kill();
  server.close();
}
