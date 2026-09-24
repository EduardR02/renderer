/**
 * Starts the owner's real app (target/release/renderer.exe) with the WebView2
 * DevTools port open, so the harness bridge in the dev server can read from
 * it. Or reports that it is already reachable.
 *
 *   bun dev/real-app.js            port 9341
 *   REAL_APP_CDP_PORT=9400 bun dev/real-app.js   (give the dev server the same variable)
 *
 * The port is opened with WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS on this one
 * process. WebView2 appends that variable to the arguments the app sets itself
 * (`additionalBrowserArgs` in tauri.conf.json), so the release build is used
 * unchanged. Anything on this machine can drive a page through that port, so
 * it only exists while an app started by this script is running.
 *
 * The app is single-instance. If it is already running without the port this
 * script says so and stops: it never closes a process the owner started, and
 * launching a second copy would only bring the first one's window forward.
 */

import { spawn, execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { CDP_PORT } from "./real-bridge.js";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const exe = path.join(root, "target", "release", "renderer.exe");
const harness = "http://127.0.0.1:1420/dev/ui-harness.html?real";
const TAURI_PAGE = /^(https?:\/\/tauri\.localhost|tauri:\/\/localhost)/;

async function tauriPage() {
  try {
    const response = await fetch(`http://127.0.0.1:${CDP_PORT}/json/list`, {
      signal: AbortSignal.timeout(1_000),
    });
    const targets = await response.json();
    return { answered: true, page: targets.find((t) => t.type === "page" && TAURI_PAGE.test(t.url)) ?? null };
  } catch {
    return { answered: false, page: null };
  }
}

function runningRenderers() {
  try {
    const csv = execFileSync("tasklist", ["/FI", "IMAGENAME eq renderer.exe", "/FO", "CSV", "/NH"], {
      encoding: "utf8",
    });
    return csv
      .split(/\r?\n/)
      .map((line) => line.match(/^"renderer\.exe","(\d+)"/i)?.[1])
      .filter(Boolean);
  } catch {
    return [];
  }
}

function done(code, message) {
  console.log(message);
  process.exit(code);
}

const before = await tauriPage();
if (before.page) {
  done(0, `Real app is reachable on DevTools port ${CDP_PORT} (${before.page.url}).\nHarness: ${harness}`);
}
if (before.answered) {
  done(1, `Something answers on port ${CDP_PORT}, but it is not the app. Pick another port with REAL_APP_CDP_PORT.`);
}

const pids = runningRenderers();
if (pids.length) {
  done(
    2,
    [
      `renderer.exe is already running (pid ${pids.join(", ")}) without DevTools on port ${CDP_PORT}.`,
      "This script never closes a running app. To read live from it, quit it yourself and run",
      "`bun dev/real-app.js` again.",
      "Meanwhile the harness answers from dev/.real-cache and the app's saved state (queue, library,",
      `recently opened playlists, history): ${harness}`,
    ].join("\n"),
  );
}

if (!fs.existsSync(exe) || !fs.existsSync(path.join(path.dirname(exe), "PlaybackEngine.exe"))) {
  done(1, `No release build at ${exe} (with PlaybackEngine.exe beside it). Build it with \`bun tauri build\`.`);
}

const inherited = process.env.WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS;
const child = spawn(exe, [], {
  cwd: path.dirname(exe),
  detached: true,
  stdio: "ignore",
  env: {
    ...process.env,
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: [inherited, `--remote-debugging-port=${CDP_PORT}`]
      .filter(Boolean)
      .join(" "),
  },
});
child.unref();
console.log(`Started ${exe} (pid ${child.pid}) with DevTools on port ${CDP_PORT}; waiting for its page...`);

const deadline = Date.now() + 30_000;
while (Date.now() < deadline) {
  const now = await tauriPage();
  if (now.page) done(0, `Real app is reachable (${now.page.url}).\nHarness: ${harness}`);
  await new Promise((resolve) => setTimeout(resolve, 400));
}
done(1, `The app started, but no Tauri page appeared on port ${CDP_PORT} within 30 s.`);
