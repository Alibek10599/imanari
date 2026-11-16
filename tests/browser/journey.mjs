import { spawn } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const [url, chromium] = process.argv.slice(2);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const profile = mkdtempSync(join(tmpdir(), "nari-journey-"));
const flags = ["--headless=new", "--remote-debugging-port=0", `--user-data-dir=${profile}`, "--no-first-run", "--no-default-browser-check", "--disable-gpu"];
if (process.env.CI && process.platform === "linux") flags.push("--no-sandbox");
const browser = spawn(chromium, [...flags, "about:blank"], { stdio: ["ignore", "ignore", "pipe"] });
let browserLog = "";
browser.stderr.on("data", (d) => {
  browserLog = (browserLog + d).slice(-4000);
});

async function waitFor(fn, label, ms = 10000) {
  const start = Date.now();
  while (Date.now() - start < ms) {
    const v = await fn();
    if (v) return v;
    await sleep(40);
  }
  throw new Error("timed out waiting for " + label + (browserLog ? "\nbrowser stderr:\n" + browserLog.trim() : ""));
}

function check(name, ok, detail) {
  results.push({ name, ok: Boolean(ok), detail: detail === undefined ? null : detail });
}

let ws;
try {
  const portFile = join(profile, "DevToolsActivePort");
  const devPort = await waitFor(() => existsSync(portFile) && readFileSync(portFile, "utf8").split("\n")[0], "devtools", 60000);
  const targets = await (await fetch(`http://127.0.0.1:${devPort}/json/list`)).json();
  ws = new WebSocket(targets.find((t) => t.type === "page").webSocketDebuggerUrl);
  await new Promise((res, rej) => {
    ws.onopen = res;
    ws.onerror = rej;
  });
  let seq = 0;
  const pending = new Map();
  const events = [];
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    if (msg.id && pending.has(msg.id)) {
      const { res, rej } = pending.get(msg.id);
      pending.delete(msg.id);
      if (msg.error) rej(new Error(JSON.stringify(msg.error)));
      else res(msg.result);
    } else events.push(msg);
  };
  const send = (method, params = {}) =>
    new Promise((res, rej) => {
      seq += 1;
      pending.set(seq, { res, rej });
      ws.send(JSON.stringify({ id: seq, method, params }));
    });
  const evaluate = async (expression) => {
    const r = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
    if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails));
    return r.result.value;
  };
  const key = async (k, code, keyCode, text) => {
    await send("Input.dispatchKeyEvent", { type: "keyDown", key: k, code, windowsVirtualKeyCode: keyCode, text });
    await send("Input.dispatchKeyEvent", { type: "keyUp", key: k, code, windowsVirtualKeyCode: keyCode });
  };
  const tab = () => key("Tab", "Tab", 9);
  const enter = () => key("Enter", "Enter", 13, "\r");
  const focusByKeyboard = async (predicate, label) => {
    await evaluate("document.activeElement && document.activeElement.blur(), true");
    for (let i = 0; i < 40; i++) {
      if (await evaluate(predicate)) return true;
      await tab();
    }
    throw new Error("not reachable by keyboard: " + label);
  };
  const latency = (ms) => send("Network.emulateNetworkConditions", { offline: false, latency: ms, downloadThroughput: -1, uploadThroughput: -1 });
  const text = (sel) => evaluate(`Array.from(document.querySelectorAll(${JSON.stringify(sel)})).map(e => e.textContent).join("|")`);

  await send("Page.enable");
  await send("Runtime.enable");
  await send("Network.enable");
  await latency(250);
  await send("Page.navigate", { url });
  const sawLoading = await waitFor(() => evaluate("!!document.querySelector('main .loading')"), "loading state").catch(() => false);
  check("loading state is shown while the list is fetched", sawLoading);
  await waitFor(() => evaluate("!!document.querySelector('main .empty')"), "empty state");
  check("empty state is shown for a new user", (await text("main .empty")).includes("Nothing here yet"));
  check("bootstrap token is removed from the URL", (await evaluate("location.hash")) === "");
  const labelled = await evaluate(`(() => { const i = document.querySelector('form input[name=title]'); const l = i && document.querySelector('label[for="' + i.id + '"]'); return !!(l && l.textContent === 'Task title' && i.required && i.placeholder === 'What needs doing?' && i.maxLength === 200); })()`);
  check("form control has an associated label and declared bounds", labelled);
  await latency(0);

  const malicious = '<img src=x onerror="window.__pwned=1">&amp; <b>bold</b>';
  await focusByKeyboard("document.activeElement && document.activeElement.name === 'title'", "title input");
  await send("Input.insertText", { text: malicious });
  await enter();
  await waitFor(async () => (await text("main li dd")).includes("<img"), "created item");
  check("malicious text renders as text, not markup", (await evaluate("document.querySelectorAll('main li img, main li b').length")) === 0 && (await text("main li dd")).includes(malicious));
  check("no script injected", (await evaluate("window.__pwned === undefined")) === true);
  check("form resets after success", (await evaluate("document.querySelector('form input[name=title]').value")) === "");

  await evaluate("document.querySelector('form input[name=title]').value = 'x'.repeat(201), true");
  await focusByKeyboard("document.activeElement && document.activeElement.name === 'title'", "title input");
  await enter();
  await waitFor(() => evaluate("document.querySelector('.alert').textContent.length > 0"), "error alert");
  const errorState = await evaluate(`(() => { const i = document.querySelector('form input[name=title]'); return { invalid: i.getAttribute('aria-invalid'), kept: i.value.length, alert: document.querySelector('.alert').getAttribute('role'), msg: document.querySelector('.alert').textContent }; })()`);
  check("server validation error is announced and input is kept", errorState.invalid === "true" && errorState.kept === 201 && errorState.alert === "alert", errorState);

  await latency(400);
  events.length = 0;
  await evaluate("document.querySelector('form input[name=title]').value = 'double', true");
  await focusByKeyboard("document.activeElement && document.activeElement.name === 'title'", "title input");
  await enter();
  await enter();
  await enter();
  await waitFor(async () => (await text("main li dd")).includes("double"), "double item", 15000);
  await latency(0);
  const posts = events.filter((e) => e.method === "Network.requestWillBeSent" && e.params.request.url.endsWith("/api/actions/create_task")).length;
  check("double submission is prevented while a request is pending", posts === 1, { create_requests: posts });

  await send("Page.reload");
  await waitFor(async () => (await text("main li dd")).includes("double"), "items after reload");
  check("tasks reload from SQLite after a page reload", (await evaluate("document.querySelectorAll('main li').length")) === 2);

  await focusByKeyboard("document.activeElement && document.activeElement.textContent === 'Mark complete' && document.activeElement.closest('li').textContent.includes('<img')", "complete button");
  await enter();
  await waitFor(async () => (await evaluate("Array.from(document.querySelectorAll('main li')).find(li => li.textContent.includes('<img')).textContent")).includes("Yes"), "completed item");
  check("task completes via keyboard", true);

  await evaluate(`(async () => {
    const s = (await (await fetch('/_nari/session')).json()).data.user;
    const list = (await (await fetch('/api/actions/list_tasks', { method: 'POST', headers: { 'Content-Type': 'application/json', 'X-Nari-CSRF': s.csrf }, body: '{}' })).json()).data.items;
    const t = list.find(i => i.title === 'double');
    await fetch('/api/actions/complete_task', { method: 'POST', headers: { 'Content-Type': 'application/json', 'X-Nari-CSRF': s.csrf, 'Idempotency-Key': crypto.randomUUID() }, body: JSON.stringify({ id: t.id, revision: t.revision }) });
    return true;
  })()`);
  await focusByKeyboard("document.activeElement && document.activeElement.textContent === 'Mark complete' && document.activeElement.closest('li').textContent.includes('double')", "stale complete button");
  await enter();
  await waitFor(() => evaluate("document.querySelector('.alert').textContent.includes('changed since it was loaded')"), "conflict message");
  check("stale revision shows a conflict instead of overwriting", true);

  await evaluate("(() => { const s = document.querySelector('#dev-bar select'); s.value = 'b'; s.dispatchEvent(new Event('change')); return true; })()");
  await waitFor(() => evaluate("!!document.querySelector('main .empty')"), "user B empty list");
  check("switching to user B shows none of user A's tasks", (await evaluate("document.querySelectorAll('main li').length")) === 0);
} catch (e) {
  check("journey completed without errors", false, String(e && e.stack ? e.stack : e));
} finally {
  try {
    if (ws) ws.close();
  } catch (e) {}
  browser.kill("SIGKILL");
  await sleep(200);
  rmSync(profile, { recursive: true, force: true });
}
console.log(JSON.stringify({ ok: results.every((r) => r.ok), results }, null, 2));
process.exit(results.every((r) => r.ok) ? 0 : 1);
