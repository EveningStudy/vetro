import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { createHash, X509Certificate } from "node:crypto";
import { once } from "node:events";
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { rm } from "node:fs/promises";
import { createServer, request as requestHttp } from "node:http";
import { createServer as createHttpsServer } from "node:https";
import { connect } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../../..", import.meta.url));
const browserPath = process.env.CHROME_PATH || (process.platform === "win32"
  ? "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe"
  : process.platform === "darwin"
    ? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
    : "/usr/bin/google-chrome");
assert(existsSync(browserPath), "Set CHROME_PATH to a Chromium or Edge executable");

const cargo = spawnSync("cargo", [
  "test", "--locked", "-p", "verre-core", "--all-features", "--test", "websocket",
  "--no-run", "--message-format=json",
], { cwd: root, encoding: "utf8", maxBuffer: 32 * 1024 * 1024, stdio: ["ignore", "pipe", "inherit"] });
if (cargo.error) throw cargo.error;
const cargoMessages = cargo.stdout.trim().split("\n").filter(Boolean).map((line) => JSON.parse(line));
if (cargo.status !== 0) {
  for (const entry of cargoMessages) if (entry.message?.rendered) process.stderr.write(entry.message.rendered);
  throw new Error(`Cargo fixture build exited with ${cargo.status}`);
}
const fixturePath = cargoMessages
  .find((entry) => entry.reason === "compiler-artifact" && entry.target.name === "websocket")?.executable;
assert(fixturePath, "Cargo did not produce the WebSocket fixture");

const fixture = spawn(fixturePath, ["--browser-fixture"], { cwd: root, stdio: ["pipe", "pipe", "inherit"] });
const fixtureExited = once(fixture, "exit");
const profile = mkdtempSync(join(tmpdir(), "verre-browser-"));
let browser;
let browserExited;
let socket;
let sessionId;
let send;
let testError;
let voiceServer;
let remoteProxy;

async function startRemoteProxy(endpoint) {
  const host = "verre-proxy.test";
  const key = join(profile, "proxy.key");
  const cert = join(profile, "proxy.crt");
  const config = join(profile, "proxy.cnf");
  writeFileSync(config, `[req]\ndistinguished_name=dn\nx509_extensions=ext\nprompt=no\n[dn]\nCN=${host}\n[ext]\nsubjectAltName=DNS:${host}\n`);
  const openssl = spawnSync("openssl", ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
    "-keyout", key, "-out", cert, "-config", config], { encoding: "utf8", windowsHide: true });
  if (openssl.error) throw openssl.error;
  assert.equal(openssl.status, 0, openssl.stderr);
  const certificate = readFileSync(cert);
  const publicKey = new X509Certificate(certificate).publicKey.export({ type: "spki", format: "der" });
  const fingerprint = createHash("sha256").update(publicKey).digest("base64");
  let stallPair = false;
  let dropPairReply = false;
  const proxy = createHttpsServer({ cert: certificate, key: readFileSync(key) }, (incoming, outgoing) => {
    if (stallPair && incoming.method === "POST" && incoming.url === "/api/auth/pair") {
      stallPair = false;
      incoming.resume();
      return;
    }
    const headers = { ...incoming.headers, "x-forwarded-for": incoming.socket.remoteAddress, "x-forwarded-proto": "https" };
    const upstream = requestHttp(new URL(incoming.url, endpoint.origin), { headers, method: incoming.method }, (response) => {
      if (dropPairReply && incoming.method === "POST" && incoming.url === "/api/auth/pair" && response.statusCode === 200) {
        dropPairReply = false;
        response.resume();
        return;
      }
      outgoing.writeHead(response.statusCode, response.headers);
      response.pipe(outgoing);
    });
    upstream.on("error", () => outgoing.destroy());
    incoming.pipe(upstream);
  });
  const websockets = new Map();
  proxy.on("upgrade", (request, socket, head) => {
    const upstream = connect(Number(endpoint.port), endpoint.hostname);
    const close = () => {
      websockets.delete(socket);
      upstream.destroy();
      socket.destroy();
    };
    websockets.set(socket, upstream);
    socket.once("close", close).on("error", close);
    upstream.once("close", close).on("error", close);
    const headers = request.rawHeaders.map((part, index) => index % 2 === 0 ? `${part}: ` : `${part}\r\n`).join("");
    upstream.write(`GET ${request.url} HTTP/1.1\r\n${headers}\r\n`);
    if (head.length) upstream.write(head);
    socket.pipe(upstream).pipe(socket);
  });
  proxy.listen(0, "127.0.0.1");
  await once(proxy, "listening");
  return {
    origin: `https://${host}:${proxy.address().port}`,
    browserFlags: [`--host-resolver-rules=MAP ${host} 127.0.0.1`, `--ignore-certificate-errors-spki-list=${fingerprint}`, "--no-proxy-server"],
    stallNextPair: () => { stallPair = true; },
    dropNextPairReply: () => { dropPairReply = true; },
    stall: () => {
      for (const [socket, upstream] of websockets) {
        socket.pause();
        upstream.pause();
      }
    },
    disconnect: () => { for (const socket of websockets.keys()) socket.destroy(); },
    close: () => new Promise((resolve) => {
      for (const socket of websockets.keys()) socket.destroy();
      proxy.close(resolve);
      proxy.closeAllConnections();
    }),
  };
}

async function waitFor(check, description, timeout = 10_000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const result = await Promise.resolve().then(check).catch((error) => {
      if (!/^Runtime\.evaluate: (Inspected target navigated or closed|Cannot find default execution context)/.test(error.message)) throw error;
    });
    if (result) return result;
    await delay(50);
  }
  throw new Error(`Timed out waiting for ${description}`);
}

try {
  const lines = createInterface({ input: fixture.stdout });
  const [url] = await once(lines, "line", { signal: AbortSignal.timeout(10_000) });
  const endpoint = new URL(url);
  if (process.env.VERRE_SMOKE_TLS_PROXY === "1") remoteProxy = await startRemoteProxy(endpoint);
  browser = spawn(browserPath, [
    "--headless", "--disable-gpu", "--no-first-run", "--remote-debugging-port=0",
    "--disable-back-forward-cache",
    "--use-fake-device-for-media-stream", "--use-fake-ui-for-media-stream",
    ...(remoteProxy?.browserFlags ?? []),
    `--user-data-dir=${profile}`, "about:blank",
  ], { stdio: "ignore" });
  browserExited = once(browser, "exit");
  const portFile = join(profile, "DevToolsActivePort");
  const [port, devtools] = await waitFor(() => {
    if (!existsSync(portFile)) return null;
    try {
      const parts = readFileSync(portFile, "utf8").trim().split(/\r?\n/);
      return parts.length === 2 ? parts : null;
    } catch (error) {
      if (error.code === "EBUSY" || error.code === "ENOENT" || error.code === "EACCES") return null;
      throw error;
    }
  }, "Chromium");
  socket = new WebSocket(`ws://127.0.0.1:${port}${devtools}`);
  await once(socket, "open", { signal: AbortSignal.timeout(10_000) });
  let nextId = 0;
  const pending = new Map();
  socket.addEventListener("message", ({ data }) => {
    const frame = JSON.parse(data);
    pending.get(frame.id)?.(frame);
  });
  send = (method, params = {}, target = sessionId) => new Promise((resolve, reject) => {
    const id = ++nextId;
    const timeout = setTimeout(() => {
      pending.delete(id);
      reject(new Error(`DevTools timed out on ${method}`));
    }, 8_000);
    pending.set(id, (frame) => {
      clearTimeout(timeout);
      pending.delete(id);
      if (frame.error) reject(new Error(`${method}: ${frame.error.message}`));
      else resolve(frame.result);
    });
    socket.send(JSON.stringify({ id, method, params, ...(target ? { sessionId: target } : {}) }));
  });

  const { targetId } = await send("Target.createTarget", { url: "about:blank" });
  ({ sessionId } = await send("Target.attachToTarget", { targetId, flatten: true }));
  await send("Page.enable");
  await send("Emulation.setDeviceMetricsOverride", { width: 1440, height: 900, deviceScaleFactor: 1, mobile: false });
  await send("Browser.grantPermissions", { origin: endpoint.origin, permissions: ["clipboardReadWrite", "clipboardSanitizedWrite"] }, null);

  const evaluate = async (expression) => {
    const { result, exceptionDetails } = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
    if (exceptionDetails) throw new Error(exceptionDetails.exception?.description ?? exceptionDetails.text);
    return result.value;
  };
  const textOnScreen = (text) => waitFor(() => evaluate(`document.body?.innerText.includes(${JSON.stringify(text)})`), text);
  const visiblePoint = (selector, text) => {
    let previous;
    return waitFor(async () => {
      const point = await evaluate(`(() => {
        const element = [...document.querySelectorAll(${JSON.stringify(selector)})].find((node) =>
          !node.disabled && node.getAttribute("aria-disabled") !== "true" && node.getClientRects().length &&
          (${JSON.stringify(text ?? null)} === null || node.innerText.trim().startsWith(${JSON.stringify(text ?? "")})));
        if (!element) return null;
        element.scrollIntoView({ block: "nearest", behavior: "instant" });
        const bounds = element.getBoundingClientRect();
        const x = bounds.x + bounds.width / 2;
        const y = bounds.y + bounds.height / 2;
        return element.contains(document.elementFromPoint(x, y)) ? { x, y } : null;
      })()`);
      const stationary = point && point.x === previous?.x && point.y === previous.y;
      previous = point;
      return stationary ? point : null;
    }, text ?? selector);
  };
  const click = async (selector, text) => {
    const point = await visiblePoint(selector, text);
    await send("Input.dispatchMouseEvent", { type: "mousePressed", button: "left", clickCount: 1, ...point });
    await send("Input.dispatchMouseEvent", { type: "mouseReleased", button: "left", clickCount: 1, ...point });
  };
  const tap = async (selector, text) => {
    const point = await visiblePoint(selector, text);
    await send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [point] });
    await send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
  };
  const press = async (key, windowsVirtualKeyCode) => {
    await send("Input.dispatchKeyEvent", { type: "keyDown", key, code: key, windowsVirtualKeyCode });
    await send("Input.dispatchKeyEvent", { type: "keyUp", key, code: key, windowsVirtualKeyCode });
  };
  const sendPrompt = async (text) => {
    await click("textarea");
    await send("Input.insertText", { text });
    await press("Enter", 13);
  };
  const copyPairLink = async (origin, previous) => {
    await click("button", "Pair a device");
    await click("button", "Copy pairing link");
    return waitFor(async () => {
      const text = await evaluate("navigator.clipboard.readText()");
      return text.startsWith(origin) && text !== previous ? text : null;
    }, "pairing link copied");
  };

  const { identifier: authFault } = await send("Page.addScriptToEvaluateOnNewDocument", { source: `(() => {
    const fetch = window.fetch.bind(window);
    const timeout = window.setTimeout.bind(window);
    window.bootstrapAttempts = 0;
    window.setTimeout = (callback, delay, ...args) =>
      timeout(callback, window.bootstrapAttempts === 2 && delay === 15000 ? 300 : delay, ...args);
    window.fetch = (input, init) => {
      if (input === "/api/auth/local") {
        window.bootstrapAttempts++;
        if (window.bootstrapAttempts === 1) return Promise.resolve(new Response(null, { status: 503 }));
        if (window.bootstrapAttempts === 2) return Promise.reject(new TypeError("Temporary connection failure"));
        if (window.bootstrapAttempts === 3) return Promise.resolve({
          ok: true,
          status: 200,
          json: () => new Promise((_, reject) => {
            init?.signal?.addEventListener("abort", () => reject(init.signal.reason), { once: true });
          }),
        });
      }
      return fetch(input, init);
    };
  })()` });
  await send("Page.navigate", { url: new URL(`/new?agent=fake&lang=en${endpoint.hash}`, endpoint).href });
  await textOnScreen("Connection lost. Reconnecting…");
  assert.equal(await evaluate("location.hash"), "", "redact the local token before exchanging it");
  await click("button", "Retry");
  await waitFor(() => evaluate("window.bootstrapAttempts === 2 && Boolean([...document.querySelectorAll('button')].find((node) => node.innerText.trim() === 'Retry'))"), "retry after a network failure");
  await click("button", "Retry");
  await waitFor(() => evaluate("window.bootstrapAttempts === 3 && Boolean([...document.querySelectorAll('button')].find((node) => node.innerText.trim() === 'Retry'))"), "retry after a stalled authentication response");
  await click("button", "Retry");
  await textOnScreen("Sign in required");
  assert.equal(await evaluate("window.bootstrapAttempts"), 4);
  await send("Page.removeScriptToEvaluateOnNewDocument", { identifier: authFault });

  const localSession = sessionId;
  let remoteSignIn;
  if (remoteProxy) {
    await send("Page.navigate", { url: new URL("/settings/remote?lang=en", endpoint).href });
    await click("#remote-proxy-url");
    await send("Input.insertText", { text: remoteProxy.origin });
    await click('form button[type="submit"]', "Start");
    await textOnScreen(remoteProxy.origin);
    const original = new URL(await copyPairLink(remoteProxy.origin));
    const remoteNewThread = new URL(`/new?agent=fake&lang=en${original.hash}`, original);
    const { targetId } = await send("Target.createTarget", { url: "about:blank" }, null);
    ({ sessionId } = await send("Target.attachToTarget", { targetId, flatten: true }, null));
    remoteSignIn = { targetId, sessionId };
    await send("Page.bringToFront");
    await send("Emulation.setDeviceMetricsOverride", { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
    await send("Page.navigate", { url: remoteNewThread.href });
    await click("button", "Connect");
    await textOnScreen("Sign-in runs on the computer running Verre.");
    assert.equal(await evaluate(`[...document.querySelectorAll("button")].some((button) => button.innerText.trim() === "Fake device code")`), false);
    await evaluate(`(() => {
      const transmit = WebSocket.prototype.send;
      WebSocket.prototype.send = function (message) {
        if (JSON.parse(message).method === "agents.refresh") window.checkedHostSignIn = true;
        transmit.call(this, message);
      };
    })()`);
    await tap("button", "Check now");
    await waitFor(() => evaluate("window.checkedHostSignIn === true"), "remote agent sign-in check");
    await textOnScreen("Sign in required");
    sessionId = localSession;
    await send("Page.bringToFront");
    await send("Page.navigate", { url: new URL("/new?agent=fake&lang=en", endpoint).href });
  }
  await click("button", "Fake device code");
  await visiblePoint("a", "https://example.test/device");
  await textOnScreen("TEST-CODE");
  await click('button[aria-label="Copy"]');
  await waitFor(async () => (await evaluate("navigator.clipboard.readText()")) === "TEST-CODE", "copied device code");
  await visiblePoint("button", "Check now");
  const { targetId: providerTab } = await send("Target.createTarget", { url: "about:blank" }, null);
  const provider = await send("Target.attachToTarget", { targetId: providerTab, flatten: true }, null);
  await send("Page.bringToFront", {}, provider.sessionId);
  await waitFor(() => evaluate("!document.hasFocus()"), "left Verre to complete sign-in");
  const completed = once(lines, "line", { signal: AbortSignal.timeout(5000) });
  fixture.stdin.write("complete-sign-in\n");
  assert.deepEqual(await completed, ["sign-in completed"]);
  await send("Page.bringToFront");
  await visiblePoint("button", "Options…");
  await send("Target.closeTarget", { targetId: providerTab }, null);
  if (remoteSignIn) {
    sessionId = remoteSignIn.sessionId;
    await send("Page.bringToFront");
    await visiblePoint("button", "Options…");
    await send("Page.navigate", { url: `${remoteProxy.origin}/settings/remote?lang=en` });
    await tap("button", "Revoke");
    await textOnScreen("Connect to Verre");
    await send("Target.closeTarget", { targetId: remoteSignIn.targetId }, null);
    sessionId = localSession;
    await send("Page.bringToFront");
  }
  await click("button", "Options…");
  assert.equal(await evaluate(`document.body.innerText.includes("Sign in required")`), false);
  await visiblePoint('button[aria-label="Model"]', "Native");
  const imagePath = join(profile, "attachment.png");
  writeFileSync(imagePath, Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=", "base64"));
  const { root: documentNode } = await send("DOM.getDocument");
  const { nodeId: attachmentInput } = await send("DOM.querySelector", { nodeId: documentNode.nodeId, selector: 'input[type="file"]' });
  assert(attachmentInput, "The composer has an attachment input");
  await evaluate(`(() => {
    const transfer = new DataTransfer();
    transfer.items.add(new File(["test"], "remote-note.txt", { type: "text/plain" }));
    document.querySelector("textarea").dispatchEvent(new DragEvent("drop", { bubbles: true, cancelable: true, dataTransfer: transfer }));
  })()`);
  await textOnScreen("Add other files from the desktop app on this machine.");
  await send("DOM.setFileInputFiles", { nodeId: attachmentInput, files: [imagePath] });
  await waitFor(() => evaluate(`document.querySelector('img[src^="data:image/png"]')?.naturalWidth === 1`), "attachment preview");
  await sendPrompt("Browser regression prompt");
  await textOnScreen("Confirm?");
  await visiblePoint('button[aria-label="Stop"]');
  await waitFor(() => evaluate(`Boolean(document.querySelector('[role="img"][aria-label*="5h"][aria-label*="7d"]'))`), "both short rate-limit labels");
  await waitFor(() => evaluate(`document.querySelector('img[alt="Image"]')?.naturalWidth === 1`), "sent attachment");
  await textOnScreen("Fake stream");
  await sendPrompt("Follow-up while approval is pending");
  await textOnScreen("Queued");
  await click("button", "Allow once");
  await textOnScreen("Follow-up while approval is pending");
  await waitFor(() => evaluate(`!document.body.innerText.includes("Queued")`), "the queued message to be delivered");
  await click('button[aria-label="Stop"]');
  await click('button[aria-label="Copy response"]');
  await waitFor(async () => (await evaluate("navigator.clipboard.readText()")) === "Fake stream", "copied response");
  const duration = await visiblePoint(".group\\/turn-footer span", "4s");
  await send("Input.dispatchMouseEvent", { type: "mouseMoved", ...duration });
  await textOnScreen("12 in · 7 out");
  await click('button[aria-label="More actions"]');
  await click('[role="menuitem"]', "Release agent process");
  await textOnScreen("Background work is still running");
  await click('[role="dialog"] button', "Cancel");
  await waitFor(() => evaluate(`!document.querySelector('[role="dialog"]')`), "dismissed release confirmation");
  await click('button[aria-label="More actions"]');
  await click('[role="menuitem"]', "Release agent process");
  await click('[role="dialog"] button', "Release and end them");
  await textOnScreen("Agent process released.");
  await sendPrompt("Resume after releasing");
  await textOnScreen("Resume after releasing");
  await click('button[aria-label="Stop"]');
  await click("button", "Search");
  await click('[role="dialog"] input');
  await send("Input.insertText", { text: "Browser regression prompt" });
  await waitFor(() => evaluate(`document.querySelector('[role="dialog"] mark')?.textContent === "Browser regression prompt"`), "full-text search result");
  await press("Escape", 27);
  await waitFor(() => evaluate(`!document.querySelector('[role="dialog"]')`), "closed search dialog");
  await click('button[aria-label="Settings"]');
  await waitFor(() => evaluate(`location.pathname.startsWith("/settings")`), "settings view");
  await send("Page.navigate", { url: new URL("/t/fake%3Alive?lang=en", endpoint).href });
  await textOnScreen("Resume after releasing");
  await send("Page.reload");
  await textOnScreen("Fake stream");
  await textOnScreen("Follow-up while approval is pending");
  assert.equal(await evaluate(`document.body.innerText.split("Browser regression prompt").length - 1`), 1);
  await sendPrompt("Keep an active thread visible through filters");
  await waitFor(() => evaluate(`Boolean(document.querySelector('button[aria-label="Stop"]'))`), "running turn");
  await click('button[aria-label="More actions"]');
  await click('[role="menuitem"]', "Archive");
  await textOnScreen("Archived");
  await click('button[aria-label="Filter"]');
  await click('[role="menuitemcheckbox"]', "CLI");
  await waitFor(() => evaluate(`document.querySelector('[role="menuitemcheckbox"][aria-checked="true"]')?.innerText.trim() === "CLI"`), "CLI history filter");
  await send("Emulation.setDeviceMetricsOverride", { width: 390, height: 844, deviceScaleFactor: 2, mobile: true });
  await send("Emulation.setTouchEmulationEnabled", { enabled: true, maxTouchPoints: 1 });
  await send("Page.navigate", { url: new URL("/new?agent=fake&lang=en", endpoint).href });
  await tap('button[aria-label="Model"]');
  await tap('[role="menuitemradio"]', "Native");
  await visiblePoint('button[aria-label="Model"]', "Native");
  await tap('button[aria-label="Model"]');
  await tap('[role="menuitemradio"]', "Chosen");
  await visiblePoint('button[aria-label="Model"]', "Chosen");
  await tap('button[aria-label="Toggle sidebar"]');
  await textOnScreen("Active");
  await tap('[role="dialog"] button', "Agent title");
  await textOnScreen("Keep an active thread visible through filters");
  await tap('button[aria-label="Stop"]');
  await waitFor(() => evaluate(`document.documentElement.scrollWidth <= window.innerWidth`), "mobile viewport without horizontal overflow");
  await send("Emulation.setTouchEmulationEnabled", { enabled: false });
  await send("Emulation.setDeviceMetricsOverride", { width: 1440, height: 900, deviceScaleFactor: 1, mobile: false });
  const notePath = join(profile, "attachment notes.txt");
  writeFileSync(notePath, "Path attachment fixture");
  await send("Page.addScriptToEvaluateOnNewDocument", { source: `(() => {
    const callbacks = new Map();
    const subscriptions = new Map();
    let nextId = 0;
    window.__VERRE_DESKTOP__ = { platform: "windows", chrome: "custom" };
    window.__TAURI_INTERNALS__ = {
      transformCallback(callback) {
        callbacks.set(++nextId, callback);
        return nextId;
      },
      async invoke(method, args) {
        if (method === "plugin:event|listen") {
          subscriptions.set(args.handler, args.event);
          return args.handler;
        }
        if (method === "plugin:event|unlisten") subscriptions.delete(args.eventId);
        if (method === "plugin:dialog|open") {
          return new Promise((resolve) => {
            window.finishFilePicker = () => resolve(${JSON.stringify([notePath])});
          });
        }
      }
    };
    const dropHandlers = () => [...subscriptions]
      .filter(([, event]) => event === "tauri://drag-drop")
      .map(([id]) => callbacks.get(id));
    window.desktopDropReady = () => dropHandlers().length > 0;
    window.dropDesktopFiles = (paths) => {
      for (const handleDrop of dropHandlers()) handleDrop({ payload: { paths } });
    };
  })()` });
  await send("Page.navigate", { url: new URL("/t/fake%3Alive?lang=en", endpoint).href });
  await textOnScreen("Keep an active thread visible through filters");
  await waitFor(() => evaluate("window.desktopDropReady?.()"), "desktop file drop subscription");
  await evaluate(`dropDesktopFiles(${JSON.stringify([imagePath])})`);
  const composerImage = `document.querySelector('img[alt=""][src^="data:image/png"]')?.naturalWidth === 1`;
  await waitFor(() => evaluate(composerImage), "native bridge image preview");
  await click('button[aria-label="Add"]');
  await click('[role="menuitem"]', "Attach");
  await waitFor(() => evaluate(`typeof finishFilePicker === "function"`), "native file picker");
  await click("button", "New thread");
  await waitFor(() => evaluate(`location.pathname.startsWith("/new")`), "new thread view");
  await evaluate(`finishFilePicker()`);
  assert.equal(await evaluate(`document.querySelector("textarea")?.value ?? ""`), "", "The file picker must not edit the new draft");
  await evaluate(`history.back()`);
  await waitFor(() => evaluate(composerImage), "original attachment after navigating back");
  await waitFor(() => evaluate(`document.querySelector("textarea")?.value.includes(${JSON.stringify(`@${notePath}`)})`), "native file mention in the original draft");
  await click("textarea");
  await press("End", 35);
  await send("Input.insertText", { text: "Desktop attachment regression" });
  await press("Enter", 13);
  await textOnScreen("Desktop attachment regression");
  await waitFor(() => evaluate(`[...document.querySelectorAll(".truncate")].some((node) => node.textContent === ${JSON.stringify(notePath)})`), "sent path attachment");
  await click('button[aria-label="Stop"]');
  await sendPrompt("Request approval before stop");
  await visiblePoint("[data-docked-request] button", "Allow once");
  await click('button[aria-label="Stop"]');
  await waitFor(() => evaluate(`!document.querySelector('[data-docked-request], button[aria-label="Stop"]')`), "cancelled approval and idle composer");

  const recordings = [];
  voiceServer = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    recordings.push({ method: request.method, path: request.url, body: Buffer.concat(chunks) });
    response.writeHead(200, { "Content-Type": "application/json" });
    response.end(JSON.stringify({ text: "verre voice input" }));
  }).listen(0, "127.0.0.1");
  await once(voiceServer, "listening");
  await send("Page.addScriptToEvaluateOnNewDocument", { source: `(() => {
    const getUserMedia = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices);
    const requests = window.testMicrophone = [];
    navigator.mediaDevices.getUserMedia = async (constraints) => {
      const request = {};
      requests.push(request);
      await new Promise((resolve) => { request.allow = resolve; });
      const stream = await getUserMedia(constraints);
      request.tracks = stream.getTracks();
      return stream;
    };
  })()` });
  await send("Page.navigate", { url: new URL("/settings/voice?lang=en", endpoint).href });
  const transcriptionUrl = `http://127.0.0.1:${voiceServer.address().port}/v1/audio/transcriptions`;
  await click('input[placeholder*="/v1/audio/transcriptions"]');
  await send("Input.insertText", { text: transcriptionUrl });
  assert.equal(await evaluate("document.activeElement.value"), transcriptionUrl, "Entered transcription URL");
  await click('textarea[placeholder*="Claude Code"]');
  await send("Input.insertText", { text: "verre = Verre" });
  assert.equal(await evaluate("document.activeElement.value"), "verre = Verre", "Entered glossary");
  await click("button", "Save");
  await textOnScreen("Local endpoint");
  await send("Page.reload");
  await waitFor(() => evaluate(`(() => {
    const endpoint = document.querySelector('input[placeholder*="/v1/audio/transcriptions"]');
    const glossary = document.querySelector('textarea[placeholder*="Claude Code"]');
    return endpoint?.value === ${JSON.stringify(transcriptionUrl)} && glossary?.value === "verre = Verre";
  })()`), "saved voice settings after reload");
  await textOnScreen("Local endpoint");
  await click("button", "New thread");
  const dictate = 'button[aria-label="Voice input"]';
  await click(dictate);
  await waitFor(() => evaluate(`testMicrophone.length === 1`), "pending microphone permission");
  await click('button[aria-label="Settings"]');
  await waitFor(() => evaluate(`location.pathname.startsWith("/settings")`), "navigation while requesting the microphone");
  await evaluate(`testMicrophone[0].allow()`);
  await waitFor(() => evaluate(`testMicrophone[0].tracks?.[0].readyState === "ended"`), "microphone released after leaving the composer");
  assert.equal(recordings.length, 0, "Abandoned microphone access must never submit audio");
  await click("button", "New thread");
  await click(dictate);
  await waitFor(() => evaluate(`testMicrophone.length === 2`), "second microphone request");
  await evaluate(`document.querySelector(${JSON.stringify(dictate)})?.click()`);
  assert.equal(await evaluate(`testMicrophone.length`), 2, "An unsettled permission request must only start one recording");
  await evaluate(`testMicrophone[1].allow()`);
  const finish = 'button[aria-label="Stop & transcribe"]';
  await visiblePoint(finish);
  await delay(800);
  await click(finish);
  await waitFor(() => evaluate(`document.querySelector("textarea")?.value === "Verre voice input"`), "transcribed draft and glossary replacement");
  assert.equal(recordings.length, 1);
  assert.equal(recordings[0].method, "POST");
  assert.equal(recordings[0].path, "/v1/audio/transcriptions");
  const wav = recordings[0].body.subarray(recordings[0].body.indexOf("RIFF"));
  assert.equal(wav.toString("ascii", 8, 12), "WAVE");
  assert.equal(wav.readUInt32LE(24), 16000, "The uploaded audio is resampled to 16 kHz");
  assert(wav.readUInt32LE(40) > 0, "The recording contains PCM samples");
  assert.equal(await evaluate(`testMicrophone[1].tracks[0].readyState`), "ended");
  await click(dictate);
  await waitFor(() => evaluate(`testMicrophone.length === 3`), "microphone request before cancellation");
  await evaluate(`testMicrophone[2].allow()`);
  await visiblePoint(finish);
  await press("Escape", 27);
  await waitFor(() => evaluate(`testMicrophone[2].tracks[0].readyState === "ended"`), "cancelled recording released");
  assert.equal(recordings.length, 1, "Cancelling a recording must never submit audio");
  await evaluate(`window.AudioContext = class { constructor() { throw new Error("Audio engine unavailable"); } }`);
  await click(dictate);
  await waitFor(() => evaluate(`testMicrophone.length === 4`), "microphone request before audio setup failure");
  await evaluate(`testMicrophone[3].allow()`);
  await textOnScreen("Audio engine unavailable");
  assert.equal(await evaluate(`testMicrophone[3].tracks[0].readyState`), "ended", "Audio setup failure must release the microphone");
  console.log("Production browser smoke passed: options, attachments, draft switching, streaming, approval, queue, stop, copy, release/resume, search, settings, reload, active filters, mobile layout, and voice recording");

  if (remoteProxy) {
    const hostSession = sessionId;
    const origin = remoteProxy.origin;
    await send("Emulation.setDeviceMetricsOverride", { width: 1440, height: 900, deviceScaleFactor: 1, mobile: false });
    await send("Page.navigate", { url: new URL("/settings/remote?lang=en", endpoint).href });
    await textOnScreen(origin);
    const pairLink = await copyPairLink(origin);
    const pairingUrl = new URL(pairLink);
    assert.equal(pairingUrl.origin, origin);
    const code = new URLSearchParams(pairingUrl.hash.slice(1)).get("pair");
    assert.match(code, /^[a-f0-9]{8}$/);

    const { targetId: remoteTarget } = await send("Target.createTarget", { url: "about:blank" }, null);
    ({ sessionId } = await send("Target.attachToTarget", { targetId: remoteTarget, flatten: true }, null));
    const remoteSession = sessionId;
    await send("Page.bringToFront");
    let remoteHandshakes = 0;
    socket.addEventListener("message", ({ data }) => {
      const frame = JSON.parse(data);
      if (frame.sessionId === remoteSession && frame.method === "Network.webSocketHandshakeResponseReceived" &&
          frame.params.response.status === 101) remoteHandshakes++;
    });
    await send("Network.enable");
    await send("Page.enable");
    await send("Page.addScriptToEvaluateOnNewDocument", { source: `(() => {
      const prototype = WebSocket.prototype;
      const pending = new WeakMap();
      const transmit = prototype.send;
      prototype.send = function (message) {
        if (window.dropResponseFor || window.dropRequestFor) {
          const request = JSON.parse(message);
          if (request.method === window.dropRequestFor) {
            window.dropRequestFor = null;
            window.droppedRequest = true;
            this.close();
            return;
          }
          if (request.method === window.dropResponseFor) {
            window.dropResponseFor = null;
            pending.set(this, request.id);
          }
        }
        transmit.call(this, message);
      };
      const onmessage = Object.getOwnPropertyDescriptor(prototype, "onmessage");
      Object.defineProperty(prototype, "onmessage", {
        ...onmessage,
        set(listener) {
          onmessage.set.call(this, (event) => {
            if (pending.has(this) && JSON.parse(event.data).id === pending.get(this)) {
              window.droppedResponse = JSON.parse(event.data);
              this.close();
              return;
            }
            listener.call(this, event);
          });
        },
      });
      window.responseFaultInstalled = true;
    })()` });
    await send("Emulation.setDeviceMetricsOverride", { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
    pairingUrl.searchParams.set("lang", "en");
    await send("Page.navigate", { url: pairingUrl.href });
    await textOnScreen("Connect to Verre");
    assert(await evaluate("isSecureContext"));
    assert.equal(await evaluate("document.querySelector('#pair-code').value"), code);
    assert.equal(await evaluate("location.hash"), "");
    await click("#pair-code");
    await send("Input.insertText", { text: "0000" });
    await click("button", "Connect");
    await textOnScreen("This code is incorrect");
    await evaluate(`(() => { const input = document.querySelector('#pair-code'); input.focus(); input.select(); })()`);
    await send("Input.insertText", { text: code });
    remoteProxy.stallNextPair();
    const shortenPairTimeout = () => evaluate(`(() => {
      const timeout = window.setTimeout;
      window.setTimeout = (callback, delay, ...args) => {
        if (delay === 15000) {
          window.setTimeout = timeout;
          return timeout(callback, 750, ...args);
        }
        return timeout(callback, delay, ...args);
      };
    })()`);
    await shortenPairTimeout();
    await click("button", "Connect");
    await textOnScreen("Pairing timed out");
    assert.equal(await evaluate("document.querySelector('#pair-code').value"), code);
    remoteProxy.dropNextPairReply();
    await shortenPairTimeout();
    await click("button", "Connect");
    await textOnScreen("Pairing timed out");
    assert.equal(await evaluate("document.querySelector('#pair-code').value"), code);
    await click("button", "Connect");
    const paired = await waitFor(() => evaluate(`!document.querySelector("#pair-code") ? "connected"
      : document.body.innerText.includes("This code is incorrect") ? "code consumed" : null`), "paired device signed in");
    assert.equal(paired, "connected", "retry must recover the device from the lost pairing response");
    await send("Page.navigate", { url: `${origin}/t/fake%3Alive?lang=en` });
    await textOnScreen("Browser regression prompt");
    await sendPrompt("Request approval before stop");
    await textOnScreen("Confirm?");
    const beforeDisconnect = remoteHandshakes;
    remoteProxy.disconnect();
    await waitFor(() => remoteHandshakes > beforeDisconnect, "new WebSocket after transport loss");
    await waitFor(() => evaluate(`!document.body.innerText.includes("Reconnecting")`), "the recovered connection");
    const beforeStall = remoteHandshakes;
    remoteProxy.stall();
    await evaluate(`window.dispatchEvent(new Event("online"))`);
    await waitFor(() => remoteHandshakes > beforeStall, "new WebSocket after a silent transport stall");
    await click("button", "Allow once");
    await textOnScreen("Fake stream");
    await waitFor(() => evaluate(`!document.body.innerText.includes("Confirm?")`), "approval after reconnect");
    await waitFor(() => evaluate(`!document.querySelector('button[aria-label="Stop"]')`), "turn completion after reconnect");

    await send("Emulation.setTouchEmulationEnabled", { enabled: true, maxTouchPoints: 1 });
    await waitFor(() => evaluate(`matchMedia("(pointer: coarse)").matches`), "touch composer input mode");
    await click("textarea");
    await send("Input.insertText", { text: "First mobile draft line" });
    await send("Input.dispatchKeyEvent", { type: "keyDown", key: "Enter", code: "Enter", windowsVirtualKeyCode: 13, text: "\r" });
    await send("Input.dispatchKeyEvent", { type: "keyUp", key: "Enter", code: "Enter", windowsVirtualKeyCode: 13 });
    assert.equal(await evaluate('document.querySelector("textarea")?.value'), "First mobile draft line\n",
      "Return starts a new line in the touch composer");
    assert.equal(await evaluate(`Boolean(document.querySelector('button[aria-label="Stop"]'))`), false,
      "Return never submits the mobile draft");
    await evaluate(`document.querySelector("textarea").select()`);
    await press("Backspace", 8);
    await waitFor(() => evaluate(`document.querySelector("textarea")?.value === ""`), "cleared the multiline draft");
    await send("Emulation.setTouchEmulationEnabled", { enabled: false });

    const historyDraft = Array.from({ length: 16 }, (_, index) => `Draft line ${index + 1}`).join("\n");
    await click("textarea");
    await send("Input.insertText", { text: historyDraft });
    const sendButtonBottom = `document.querySelector('button[aria-label="Send"]')?.getBoundingClientRect().bottom`;
    const unoccludedSend = await evaluate(sendButtonBottom);
    await evaluate(`(() => {
      const viewport = window.visualViewport;
      Object.defineProperty(viewport, "height", { configurable: true, value: 400 });
      viewport.dispatchEvent(new Event("resize"));
    })()`);
    assert(await evaluate(`${sendButtonBottom} <= 400`), "The composer must fit above an on-screen keyboard");
    assert(await evaluate(`document.querySelector("textarea").getBoundingClientRect().height <= 160`),
      "A long draft must leave room for the composer controls");
    const keyboardSend = await evaluate(sendButtonBottom);
    await evaluate(`(() => {
      const viewport = window.visualViewport;
      Object.defineProperty(viewport, "offsetTop", { configurable: true, value: 36 });
      viewport.dispatchEvent(new Event("scroll"));
    })()`);
    assert(Math.abs(await evaluate(sendButtonBottom) - keyboardSend - 36) <= 1,
      "When the visual viewport pans, the composer stays within it");
    await evaluate(`delete window.visualViewport.offsetTop; delete window.visualViewport.height;
      window.visualViewport.dispatchEvent(new Event("resize"))`);
    assert(Math.abs(await evaluate(sendButtonBottom) - unoccludedSend) <= 1, "Closing the keyboard restores the layout");
    await evaluate("window.originalDraftDocument = true");
    const { currentIndex, entries } = await send("Page.getNavigationHistory");
    const beforeHistory = remoteHandshakes;
    await send("Page.navigate", { url: `${origin}/api/health` });
    await waitFor(() => evaluate(`location.pathname === "/api/health"`), "navigated away from the draft");
    await send("Page.navigateToHistoryEntry", { entryId: entries[currentIndex].id });
    await waitFor(() => evaluate(`location.pathname.startsWith("/t/") && Boolean(document.querySelector("textarea"))`), "returned to the chat");
    assert.equal(await evaluate("window.originalDraftDocument"), undefined, "the browser rebuilt the document");
    assert.equal(await evaluate(`document.querySelector("textarea")?.value`), historyDraft, "restored the draft in the rebuilt document");
    await waitFor(() => remoteHandshakes > beforeHistory, "new WebSocket after history restore");
    await evaluate(`document.querySelector("textarea").select()`);
    await press("Backspace", 8);
    await waitFor(() => evaluate(`document.querySelector("textarea")?.value === ""`), "cleared the history draft");

    const retryPrompt = "One delivery after a lost send response";
    assert.equal(await evaluate("window.responseFaultInstalled"), true);
    await evaluate('window.dropResponseFor = "threads.send"');
    await sendPrompt(retryPrompt);
    await waitFor(() => evaluate("window.droppedResponse?.result"), "lost send response");
    await waitFor(() => evaluate(`document.querySelector("textarea")?.value === ${JSON.stringify(retryPrompt)}
      && !document.body.innerText.includes("Reconnecting") && document.body.innerText.includes(${JSON.stringify(retryPrompt)})`),
      "retained draft and received message after reconnect");
    await send("Page.reload");
    await waitFor(() => evaluate(`document.querySelector("textarea")?.value === ${JSON.stringify(retryPrompt)}`),
      "pending send draft restored after reload");
    await click('button[aria-label="Send"]');
    await waitFor(() => evaluate(`document.querySelector("textarea")?.value === ""`), "acknowledged retry");
    assert.equal(await evaluate(`document.body.innerText.includes("Queued")`), false,
      "retry must not queue another copy of the accepted message");
    await click('button[aria-label="Stop"]');
    await waitFor(() => evaluate(`!document.querySelector('button[aria-label="Stop"]')`), "remote stop acknowledged");
    await send("Page.reload");
    await textOnScreen("Request approval before stop");
    await textOnScreen(retryPrompt);
    assert.equal(await evaluate(`document.body.innerText.split(${JSON.stringify(retryPrompt)}).length - 1`), 1);

    const openNewRemoteThread = async () => {
      await send("Page.navigate", { url: `${origin}/new?agent=fake&lang=en` });
      await waitFor(() => evaluate('location.pathname === "/new" && document.querySelector("main h1") !== null'),
        "new remote conversation loaded");
      await visiblePoint('button[aria-label="Model"]');
    };
    const firstPrompt = "One native session after a lost start response";
    await openNewRemoteThread();
    await evaluate('window.dropResponseFor = "threads.start"');
    await sendPrompt(firstPrompt);
    const original = await waitFor(() => evaluate("window.droppedResponse?.result?.thread.id"), "lost start response");
    assert.equal(original, "fake:live-2");
    await waitFor(() => evaluate(`document.querySelector("textarea")?.value === ${JSON.stringify(firstPrompt)}
      && !document.body.innerText.includes("Reconnecting")`), "retained new-thread draft after reconnect");
    await send("Page.reload");
    await waitFor(() => evaluate(`document.querySelector("textarea")?.value === ${JSON.stringify(firstPrompt)}`),
      "pending new-thread draft restored after reload");
    await click('button[aria-label="Send"]');
    const entered = await waitFor(() => evaluate('location.pathname.startsWith("/t/") && decodeURIComponent(location.pathname.slice(3))'),
      "opened thread after retrying its first message");
    assert.equal(entered, original, "retry must open the original native session");
    await textOnScreen(firstPrompt);
    await textOnScreen("Confirm?");
    await click("button", "Allow once");

    const undelivered = "First message with no receipt";
    await openNewRemoteThread();
    await evaluate('window.dropRequestFor = "threads.start"');
    await sendPrompt(undelivered);
    await waitFor(() => evaluate("window.droppedRequest === true"), "lost start request");
    await waitFor(() => evaluate(`!document.body.innerText.includes("Reconnecting")`), "reconnect after the missing request");
    await send("Page.reload");
    await waitFor(() => evaluate(`document.querySelector("textarea")?.value === ${JSON.stringify(undelivered)}`),
      "unconfirmed new-thread draft restored after reload");
    await click('button[aria-label="Send"]');
    await textOnScreen("Verre could not confirm delivery.");
    await visiblePoint("button", "Send anyway");
    assert.equal(await evaluate("location.pathname"), "/new");
    assert.equal(await evaluate('document.querySelector("textarea")?.value'), undelivered);
    await click("textarea");
    await press("End", 35);
    await send("Input.insertText", { text: " after checking history" });
    await click('button[aria-label="Send"]');
    await waitFor(() => evaluate('decodeURIComponent(location.pathname) === "/t/fake:live-3"'), "fresh start after editing the draft");
    await textOnScreen(`${undelivered} after checking history`);
    await click("button", "Allow once");

    const verbatimResend = "Exact resend without editing";
    await evaluate('window.droppedRequest = false; window.dropRequestFor = "threads.send"');
    await sendPrompt(verbatimResend);
    await waitFor(() => evaluate("window.droppedRequest === true"), "lost follow-up request");
    await waitFor(() => evaluate(`!document.body.innerText.includes("Reconnecting")`), "reconnect after lost follow-up");
    await send("Page.reload");
    await waitFor(() => evaluate(`document.querySelector("textarea")?.value === ${JSON.stringify(verbatimResend)}`),
      "unconfirmed follow-up draft restored after reload");
    await click('button[aria-label="Send"]');
    await textOnScreen("Verre could not confirm delivery.");
    await click("button", "Send anyway");
    await waitFor(() => evaluate(`document.querySelector("textarea")?.value === ""`), "verbatim resend submitted");
    await textOnScreen(verbatimResend);
    assert.equal(await evaluate(`document.body.innerText.split(${JSON.stringify(verbatimResend)}).length - 1`), 1);
    await click('button[aria-label="Stop"]');
    await waitFor(() => evaluate(`!document.querySelector('button[aria-label="Stop"]')`), "verbatim resend stopped");

    await send("Target.closeTarget", { targetId: remoteTarget }, null);
    const remoteThreadUrl = `${origin}/t/fake%3Alive?lang=en`;
    const openRemoteTab = async (url = remoteThreadUrl) => {
      const { targetId } = await send("Target.createTarget", { url: "about:blank" }, null);
      ({ sessionId } = await send("Target.attachToTarget", { targetId, flatten: true }, null));
      await send("Page.bringToFront");
      await send("Page.navigate", { url });
      return { targetId, sessionId };
    };
    const reopened = await openRemoteTab();
    await textOnScreen("Browser regression prompt");
    assert.equal(await evaluate('sessionStorage.getItem("verre:session")'), null, "the reopened tab starts with fresh tab storage");
    await click("textarea");
    await send("Input.insertText", { text: "Draft while remote access is stopped" });

    sessionId = hostSession;
    await send("Page.bringToFront");
    await click("button", "Stop remote access");
    await visiblePoint("#remote-proxy-url");
    sessionId = reopened.sessionId;
    await send("Page.bringToFront");
    await textOnScreen("Connection lost. Reconnecting…");
    assert(await evaluate('Boolean(localStorage.getItem("verre:remote-session"))'), "stopping access preserves the pairing");

    sessionId = hostSession;
    await send("Page.bringToFront");
    await click("#remote-proxy-url");
    await send("Input.insertText", { text: origin });
    await click('form button[type="submit"]', "Start");
    await textOnScreen(origin);
    sessionId = reopened.sessionId;
    await send("Page.bringToFront");
    await waitFor(() => evaluate(`!document.body.innerText.includes("Reconnecting")
      && document.querySelector("textarea")?.value === "Draft while remote access is stopped"`), "paired chat and draft restored after re-enabling remote access", 20_000);

    sessionId = hostSession;
    await send("Page.bringToFront");
    await waitFor(() => evaluate(`!document.body.innerText.includes(${JSON.stringify(code)})`), "consumed pairing removed");
    assert.equal(await evaluate('localStorage.getItem("verre:remote-session")'), null, "the host credential remains tab-scoped");
    await click("button", "Revoke");
    await textOnScreen("No devices paired yet.");
    sessionId = reopened.sessionId;
    await send("Page.bringToFront");
    await textOnScreen("Connect to Verre");
    assert.equal(await evaluate('localStorage.getItem("verre:remote-session")'), null, "revocation clears the remembered session");
    assert.equal(await evaluate('Object.keys(sessionStorage).some((key) => key.startsWith("verre:remote-drafts:"))'),
      false, "revocation clears the pairing's saved drafts");
    await send("Target.closeTarget", { targetId: reopened.targetId }, null);

    sessionId = hostSession;
    await send("Page.bringToFront");
    const temporaryPairing = new URL(await copyPairLink(origin, pairLink));
    temporaryPairing.searchParams.set("lang", "en");
    const temporary = await openRemoteTab(temporaryPairing.href);
    await textOnScreen("Remember this browser");
    await click('label:has(input[type="checkbox"])', "Remember this browser");
    await waitFor(() => evaluate(`document.querySelector('input[type="checkbox"]')?.checked === false`), "temporary pairing selected");
    await click("button", "Connect");
    await waitFor(() => evaluate('!document.querySelector("#pair-code")'), "temporary pairing completed");
    assert.notEqual(await evaluate('sessionStorage.getItem("verre:session")'), null, "temporary session stays in the tab");
    assert.equal(await evaluate('localStorage.getItem("verre:remote-session")'), null);
    await send("Page.navigate", { url: `${origin}/t/fake%3Alive?lang=en` });
    await textOnScreen("Browser regression prompt");
    await send("Target.closeTarget", { targetId: temporary.targetId }, null);
    await openRemoteTab();
    await textOnScreen("Connect to Verre");

    sessionId = hostSession;
    await send("Page.bringToFront");
    await click("button", "Revoke");
    await textOnScreen("No devices paired yet.");
    await click("button", "Stop remote access");
    await visiblePoint("#remote-proxy-url");
    console.log("HTTPS reverse proxy smoke passed: host sign-in, pairing recovery, chat, reconnect, keyboard viewport, history restore, send receipts, reload, reopen, stop and restart, and revoke");
  }
} catch (error) {
  testError = error;
  if (send && sessionId && socket.readyState === WebSocket.OPEN) {
    const page = await send("Runtime.evaluate", { expression: "document.body?.innerText.slice(0, 1500)", returnByValue: true }).catch(() => null);
    if (page?.result?.value) process.stderr.write(`${page.result.value}\n`);
  }
  throw error;
} finally {
  if (send && socket.readyState === WebSocket.OPEN) {
    await send("Browser.close", {}, null).catch(() => {});
  }
  socket?.close();
  if (browser) {
    const closed = await Promise.race([
      browserExited.then(() => true, () => true),
      delay(5_000, false, { ref: false }),
    ]);
    if (!closed) {
      browser.kill();
      await browserExited.catch(() => {});
    }
  }
  fixture.stdin.end();
  const stopped = await Promise.race([fixtureExited.then(() => true), delay(8_000, false, { ref: false })]);
  if (!stopped) {
    fixture.kill();
    await fixtureExited;
  }
  if (voiceServer) await new Promise((resolve) => voiceServer.close(resolve));
  if (remoteProxy) await remoteProxy.close();
  try {
    await rm(profile, { recursive: true, force: true, maxRetries: 15, retryDelay: 200 });
  } catch (cleanupError) {
    if (!testError) throw cleanupError;
    console.error(cleanupError);
  }
}
