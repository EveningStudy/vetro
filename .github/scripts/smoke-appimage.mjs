import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { once } from "node:events";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";

assert.equal(process.platform, "linux");
assert.equal(process.argv.length, 3, "Provide one Verre AppImage");
const image = resolve(process.argv[2]);
const frontend = new URL("../../apps/web/dist/", import.meta.url);
const scratch = await mkdtemp(join(tmpdir(), "verre-appimage-"));
const appHome = join(scratch, "core");
const bin = join(scratch, "bin");
const cwd = join(scratch, "workspace");
const associations = join(scratch, "data", "applications");
const endpoint = createServer();
let child;
let exited;
let socket;
let stderr = "";

async function until(check, description, timeout = 12_000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const result = await check();
    if (result) return result;
    if (child?.exitCode != null || child?.signalCode != null) {
      throw new Error(`AppImage exited with ${child.signalCode ?? child.exitCode} before ${description}`);
    }
    await delay(100);
  }
  throw new Error(`Timed out waiting for ${description}`);
}

try {
  endpoint.listen(0, "127.0.0.1");
  await once(endpoint, "listening");
  const { port } = endpoint.address();
  await new Promise((done) => endpoint.close(done));
  const origin = `http://127.0.0.1:${port}`;

  await Promise.all([appHome, bin, cwd, associations].map((dir) => mkdir(dir, { recursive: true })));
  const environment = {
    ...process.env,
    HOME: scratch,
    XDG_CACHE_HOME: join(scratch, "cache"),
    XDG_CONFIG_HOME: join(scratch, "config"),
    XDG_DATA_HOME: join(scratch, "data"),
    GDK_BACKEND: "x11",
    LIBGL_ALWAYS_SOFTWARE: "1",
    WEBKIT_DISABLE_DMABUF_RENDERER: "1",
    VERRE_HOME: appHome,
    PATH: `${bin}:${process.env.PATH}`,
  };
  if (environment.APPIMAGE_EXTRACT_AND_RUN === "1") environment.TMPDIR = scratch;
  delete environment.VERRE_DESKTOP_URL;

  const fixture = join(scratch, "native agent.py");
  const document = join(cwd, "file with spaces.txt");
  const opened = join(scratch, "opened.txt");
  await writeFile(document, "Verre package check", "utf8");
  await writeFile(fixture, `import json, os, sys
assert "APPDIR" not in os.environ
assert "APPIMAGE" not in os.environ
assert os.environ["VERRE_PACKAGE_TEST"] == "preserved"
for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    results = {
        "initialize": {"protocolVersion": 1, "agentCapabilities": {}, "authMethods": []},
        "session/new": {"sessionId": "package-smoke"},
        "session/close": {},
    }
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"],
                      "result": results[request["method"]]}), flush=True)
`);
  await writeFile(join(appHome, "config.json"), JSON.stringify({
    port,
    externalEditor: "system",
    agents: Object.fromEntries(
      ["claude", "codex", "antigravity", "devin", "kiro", "kiro-legacy"].map((id) => [id, { enabled: false }]),
    ),
    customAcpAgents: [{
      id: "package-test", name: "Package test", command: "/usr/bin/python3", args: [fixture],
      env: { VERRE_PACKAGE_TEST: "preserved" },
    }],
  }));

  const opener = join(scratch, "open-fixture.py");
  await writeFile(opener, `import os, pathlib, sys
assert "APPDIR" not in os.environ
assert "APPIMAGE" not in os.environ
pathlib.Path(${JSON.stringify(opened)}).write_text(pathlib.Path(sys.argv[1]).read_text())
`);
  await writeFile(join(associations, "verre-smoke.desktop"), [
    "[Desktop Entry]", "Type=Application", "Name=Verre test handler",
    `Exec=/usr/bin/python3 "${opener}" %f`, "MimeType=text/plain;", "NoDisplay=true",
  ].join("\n") + "\n");
  execFileSync("/usr/bin/gio", ["mime", "text/plain", "verre-smoke.desktop"], { env: environment });
  await Promise.all(["xdg-open", "powershell.exe"].map((name) =>
    writeFile(join(bin, name), "#!/bin/sh\nexit 3\n", { mode: 0o755 }),
  ));

  child = spawn(image, [], { env: environment, cwd: scratch, detached: true, stdio: ["ignore", "ignore", "pipe"] });
  exited = new Promise((done, reject) => child.once("exit", done).once("error", reject));
  exited.catch(() => {});
  child.stderr.on("data", (chunk) => { stderr = (stderr + chunk).slice(-4096); });
  const window = await until(() => execFileSync("xwininfo", ["-root", "-tree"], { encoding: "utf8" })
    .match(/^\s*(0x[\da-f]+)\s+"Verre"/m)?.[1], "native Verre window", 30_000);

  const html = await until(async () => {
    const response = await fetch(`${origin}/`, { signal: AbortSignal.timeout(1000) }).catch(() => null);
    return response?.ok && Buffer.from(await response.arrayBuffer());
  }, "embedded frontend");
  assert(html.equals(await readFile(new URL("index.html", frontend))), "The AppImage serves the current HTML");
  const entry = html.toString("utf8").match(/<script[^>]+src="(\/assets\/[^"]+)"/)?.[1];
  assert(entry, "The frontend has a JavaScript entry point");
  const script = await fetch(`${origin}${entry}`, { signal: AbortSignal.timeout(3000) });
  assert(script.ok && Buffer.from(await script.arrayBuffer()).equals(await readFile(new URL(entry.slice(1), frontend))),
    "The AppImage serves the current JavaScript");
  const { localToken: token } = JSON.parse(await readFile(join(appHome, "state.json"), "utf8"));
  const auth = await fetch(`${origin}/api/auth/local`, {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ token }),
    signal: AbortSignal.timeout(3000),
  });
  assert(auth.ok, `Local authentication returned ${auth.status}`);
  const { session } = await auth.json();
  socket = new WebSocket(`ws://127.0.0.1:${port}/api/ws`);
  await once(socket, "open", { signal: AbortSignal.timeout(3000) });
  let nextId = 0;
  function rpc(method, params) {
    const id = ++nextId;
    return new Promise((done, reject) => {
      const timeout = setTimeout(() => {
        socket.removeEventListener("message", receive);
        reject(new Error(`${method} timed out`));
      }, 8000);
      function receive({ data }) {
        const message = JSON.parse(data);
        if (message.id !== id) return;
        clearTimeout(timeout);
        socket.removeEventListener("message", receive);
        if (message.error) reject(new Error(`${method}: ${message.error.message}`));
        else done(message.result);
      }
      socket.addEventListener("message", receive);
      socket.send(JSON.stringify({ id, method, params }));
    });
  }

  await rpc("app.hello", { session, protocolVersion: 1, clientName: "Linux package smoke" });
  await rpc("agents.options", { agentId: "package-test", cwd, prepare: true });
  await rpc("workspace.openExternal", { cwd, path: "file with spaces.txt", target: "editor" });
  const content = await until(() => readFile(opened, "utf8").catch((error) => {
    if (error.code !== "ENOENT") throw error;
  }), "system MIME handler");
  assert.equal(content, "Verre package check");

  const closeWindow = join(scratch, "close-window");
  execFileSync("cc", ["-x", "c", "-", "-o", closeWindow, "-lX11"], { input: `
#include <X11/Xlib.h>
#include <stdlib.h>

int main(int argc, char **argv) {
    if (argc != 2) return 1;
    Display *display = XOpenDisplay(NULL);
    if (!display) return 1;
    Window window = strtoul(argv[1], NULL, 0);
    XEvent event = {0};
    event.xclient.type = ClientMessage;
    event.xclient.window = window;
    event.xclient.message_type = XInternAtom(display, "WM_PROTOCOLS", False);
    event.xclient.format = 32;
    event.xclient.data.l[0] = XInternAtom(display, "WM_DELETE_WINDOW", False);
    event.xclient.data.l[1] = CurrentTime;
    Status sent = XSendEvent(display, window, False, NoEventMask, &event);
    XCloseDisplay(display);
    return sent ? 0 : 1;
}
` });
  socket.close();
  execFileSync(closeWindow, [window]);
  const exitCode = await Promise.race([exited, delay(8000, "timeout", { ref: false })]);
  assert.equal(exitCode, 0, "AppImage must exit after the native window closes");
  await assert.rejects(fetch(`${origin}/api/health`, { signal: AbortSignal.timeout(1000) }));
  console.log("AppImage passed native window, embedded frontend, Python ACP, GIO file opening, and shutdown.");
} catch (error) {
  if (stderr) process.stderr.write(stderr);
  throw error;
} finally {
  socket?.close();
  if (child?.pid && child.exitCode == null && child.signalCode == null) {
    try {
      process.kill(-child.pid, "SIGTERM");
    } catch (error) {
      if (error.code !== "ESRCH") throw error;
    }
    if (!await Promise.race([exited.then(() => true), delay(4000, false, { ref: false })])) {
      try {
        process.kill(-child.pid, "SIGKILL");
      } catch (error) {
        if (error.code !== "ESRCH") throw error;
      }
      await exited;
    }
  }
  await rm(scratch, { recursive: true, force: true });
}
