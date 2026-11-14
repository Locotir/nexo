// nexo browser: the service worker. It connects to the local nexo native host, shows the person a prompt when a
// nexo session asks to control this browser, and only after they click Allow does it open tabs for that session
// (in a "nexo" tab group), attach Chrome's debugger to them and relay raw DevTools commands and events.
//
// Visible control: Chrome's own "started debugging this browser" bar, the "nexo" tab group and the toolbar badge.
// The host checks consent and tab ownership too; everything is checked again here, so neither end has to be trusted.
"use strict";

const HOST = "dev.nexo.browser";
const PROTOCOL = "1.3";
// Slice size of a big message, in UTF-16 units (a message of up to ~600 KB per slice, far below Chrome's limits).
const CHUNK = 200000;
const MAX_CHUNKS = 1024;
const MAX_PARTIAL = 8;
const MAX_MESSAGE = 48 * 1000 * 1000;
// The agent waits 120 s; the prompt gives up a little earlier so its answer arrives in time.
const CONSENT_MS = 110000;
const PING_MS = 20000;
const MAX_TABS = 20;
const GROUP_TITLE = "nexo";
const CONSENT_URL = chrome.runtime.getURL("consent.html");

// What nexo may ask the browser for. Cookies, storage, the browser itself and other targets stay out of reach.
const DOMAINS = new Set([
  "Page", "Runtime", "DOM", "Input", "Log", "Network", "Emulation", "Accessibility", "CSS", "DOMSnapshot", "Performance",
]);
const BLOCKED = new Set([
  "Network.getAllCookies", "Network.getCookies", "Network.setCookie", "Network.setCookies", "Network.deleteCookies",
  "Network.clearBrowserCookies", "Network.clearBrowserCache", "Network.setExtraHTTPHeaders", "Network.setBypassServiceWorker",
  "Page.setDownloadBehavior", "Page.setBypassCSP", "Page.addScriptToEvaluateOnNewDocument", "Page.setInterceptFileChooserDialog",
  "Page.setWebLifecycleState", "Page.crash", "Page.close",
]);
const INTERNAL = /^(chrome|chrome-untrusted|chrome-extension|chrome-search|devtools|edge|brave|vivaldi|opera):/i;
// The events the agent gets (everything else stays in the browser).
const FORWARD = new Set([
  "Runtime.consoleAPICalled", "Runtime.exceptionThrown", "Log.entryAdded",
  "Network.requestWillBeSent", "Network.responseReceived", "Network.loadingFailed",
  "Page.loadEventFired", "Page.screencastFrame", "Page.javascriptDialogOpening",
]);
const NEEDS_TAB = new Set(["tabs.close", "tabs.activate", "window.resize", "cdp"]);

let port = null;
let connecting = false;
let reconnectTimer = null;
let reconnectMs = 1000;
let pingTimer = null;
let lastPong = 0;
let outCid = 0;
let groupChain = Promise.resolve();

const sessions = new Map(); // sid -> { name, granted, revoked, tabs: Set<number>, consent: { windowId, timer } | null }
const tabOwner = new Map(); // tab id -> sid
const attached = new Set(); // tab ids the debugger is attached to
const attaching = new Map(); // tab id -> promise of a running attach
const lost = new Map(); // tab id -> why the debugger let go (never re-attached behind the person's back)
const partial = new Map(); // cid -> { n, parts, got }: host messages arriving in slices

const text = (v, max) => String(v === undefined || v === null ? "" : v).replace(/[\u0000-\u001f\u007f]/g, " ").slice(0, max);

// ---- the wire to the host ----------------------------------------------------------------------------------

function send(msg) {
  if (!port) return false;
  try {
    const body = JSON.stringify(msg);
    if (body.length > MAX_MESSAGE) return false;
    if (body.length <= CHUNK) {
      port.postMessage(msg);
      return true;
    }
    const pieces = [];
    for (let at = 0; at < body.length; ) {
      let end = Math.min(at + CHUNK, body.length);
      const last = body.charCodeAt(end - 1);
      // Never cut between the halves of a surrogate pair: each slice must be valid text on its own.
      if (end < body.length && last >= 0xd800 && last <= 0xdbff) end -= 1;
      pieces.push(body.slice(at, end));
      at = end;
    }
    if (pieces.length > MAX_CHUNKS) return false;
    const cid = ++outCid;
    pieces.forEach((d, i) => port.postMessage({ t: "chunk", cid, i, n: pieces.length, d }));
    return true;
  } catch (e) {
    return false;
  }
}

function join(c) {
  const { cid, i, n, d } = c;
  if (!Number.isInteger(cid) || !Number.isInteger(i) || !Number.isInteger(n)) return null;
  if (n < 1 || n > MAX_CHUNKS || i < 0 || i >= n || typeof d !== "string") return null;
  let p = partial.get(cid);
  if (!p) {
    if (partial.size >= MAX_PARTIAL) partial.delete(partial.keys().next().value);
    p = { n, parts: new Array(n), got: 0 };
    partial.set(cid, p);
  }
  if (p.n !== n || p.parts[i] !== undefined) {
    partial.delete(cid);
    return null;
  }
  p.parts[i] = d;
  p.got += 1;
  if (p.got < p.n) return null;
  partial.delete(cid);
  try {
    return JSON.parse(p.parts.join(""));
  } catch (e) {
    return null;
  }
}

async function identity() {
  let { nexoInstance } = await chrome.storage.local.get("nexoInstance");
  if (!/^[0-9a-f]{12}$/.test(nexoInstance || "")) {
    nexoInstance = Array.from(crypto.getRandomValues(new Uint8Array(6)), (b) => b.toString(16).padStart(2, "0")).join("");
    await chrome.storage.local.set({ nexoInstance });
  }
  const brands = ((navigator.userAgentData && navigator.userAgentData.brands) || []).map((b) => b.brand);
  const known = [["Google Chrome", "chrome"], ["Microsoft Edge", "edge"], ["Brave", "brave"], ["Opera", "opera"], ["Vivaldi", "vivaldi"], ["Chromium", "chromium"]];
  const [name, slug] = known.find(([n]) => brands.includes(n)) || ["Chrome", "chrome"];
  const os = { win: "Windows", mac: "macOS", linux: "Linux", cros: "ChromeOS", android: "Android" };
  const info = await chrome.runtime.getPlatformInfo();
  const version = (navigator.userAgent.match(/Chrome\/([\d.]+)/) || [])[1] || "";
  return { id: slug + "-" + nexoInstance, name, os: os[info.os] || "", version, ext: chrome.runtime.getManifest().version };
}

async function connect() {
  if (port || connecting) return;
  connecting = true;
  clearTimeout(reconnectTimer);
  try {
    const browser = await identity();
    const p = chrome.runtime.connectNative(HOST);
    port = p;
    lastPong = Date.now();
    p.onMessage.addListener((m) => {
      if (port === p) onHost(m);
    });
    p.onDisconnect.addListener(() => {
      const why = chrome.runtime.lastError ? chrome.runtime.lastError.message : "closed";
      if (port !== p) return;
      port = null;
      hostDown(why);
    });
    p.postMessage({ t: "hello", browser });
    clearInterval(pingTimer);
    pingTimer = setInterval(ping, PING_MS);
  } catch (e) {
    port = null;
    console.warn("nexo: could not start the host:", e && e.message ? e.message : e);
    scheduleReconnect();
  } finally {
    connecting = false;
  }
}

function ping() {
  if (!port) return;
  if (Date.now() - lastPong > 3 * PING_MS) {
    // The host stopped answering: drop it and start a fresh one.
    const p = port;
    port = null;
    try {
      p.disconnect();
    } catch (e) {
      /* already gone */
    }
    hostDown("the host stopped answering");
    return;
  }
  send({ t: "ping" });
}

function scheduleReconnect() {
  clearTimeout(reconnectTimer);
  reconnectTimer = setTimeout(connect, reconnectMs);
  reconnectMs = Math.min(reconnectMs * 2, 60000);
}

// The host went away: nothing it let in survives, so every session ends and every tab is let go.
function hostDown(why) {
  console.warn("nexo host disconnected:", why);
  clearInterval(pingTimer);
  partial.clear();
  for (const sid of [...sessions.keys()]) closeSession(sid);
  paint();
  scheduleReconnect();
}

// ---- messages from the host ----------------------------------------------------------------------------------

function onHost(msg) {
  if (!msg || typeof msg !== "object") return;
  if (msg.t === "chunk") {
    msg = join(msg);
    if (!msg) return;
  }
  switch (msg.t) {
    case "ready":
      reconnectMs = 1000;
      break;
    case "pong":
      lastPong = Date.now();
      break;
    case "session":
      if (msg.open) openSession(text(msg.sid, 40), text(msg.name, 40));
      else closeSession(text(msg.sid, 40));
      break;
    case "consent":
      askConsent(text(msg.sid, 40));
      break;
    case "consent_cancel":
      withdrawConsent(text(msg.sid, 40));
      break;
    case "req":
      onRequest(msg);
      break;
    case "error":
      console.warn("nexo host:", text(msg.why, 300));
      break;
  }
}

function openSession(sid, name) {
  if (sessions.has(sid) || !sid) return;
  sessions.set(sid, { name: name || "nexo", granted: false, revoked: false, tabs: new Set(), consent: null });
}

function closeSession(sid) {
  const s = sessions.get(sid);
  if (!s) return;
  withdrawConsent(sid);
  // The tabs stay open for the person; nexo just lets go of them (and Chrome's debugging bar goes away).
  for (const tab of [...s.tabs]) release(tab);
  sessions.delete(sid);
  paint();
}

// ---- consent ------------------------------------------------------------------------------------------------

async function askConsent(sid) {
  const s = sessions.get(sid);
  if (!s || s.consent) return;
  if (s.revoked) {
    send({ t: "consent_result", sid, ok: false, why: "the person stopped nexo in this browser; start a new session to ask again" });
    return;
  }
  const url = CONSENT_URL + "?sid=" + encodeURIComponent(sid) + "&name=" + encodeURIComponent(s.name);
  const waiting = { windowId: null, timer: setTimeout(() => answer(sid, false, "nobody answered the prompt in the browser"), CONSENT_MS) };
  s.consent = waiting;
  paint();
  try {
    const win = await chrome.windows.create({ url, type: "popup", width: 480, height: 420, focused: true });
    if (s.consent === waiting) waiting.windowId = win.id;
    else chrome.windows.remove(win.id).catch(() => {});
  } catch (e) {
    answer(sid, false, "the browser could not show the prompt: " + text(e && e.message, 120));
  }
}

function answer(sid, ok, why) {
  const s = sessions.get(sid);
  if (!s || !s.consent) return;
  const { windowId, timer } = s.consent;
  clearTimeout(timer);
  s.consent = null;
  if (windowId !== null) chrome.windows.remove(windowId).catch(() => {});
  s.granted = Boolean(ok) && !s.revoked;
  send(s.granted ? { t: "consent_result", sid, ok: true } : { t: "consent_result", sid, ok: false, why: why || "the person clicked Deny" });
  paint();
}

// The agent (or the host's timeout) withdrew the question: the prompt goes away without an answer.
function withdrawConsent(sid) {
  const s = sessions.get(sid);
  if (!s || !s.consent) return;
  clearTimeout(s.consent.timer);
  if (s.consent.windowId !== null) chrome.windows.remove(s.consent.windowId).catch(() => {});
  s.consent = null;
  paint();
}

chrome.runtime.onMessage.addListener((msg, sender, reply) => {
  // Only our own consent page, opened for that very session, can answer.
  if (!msg || msg.t !== "consent_answer" || sender.id !== chrome.runtime.id) return;
  if (!sender.url || !sender.url.startsWith(CONSENT_URL)) return;
  const sid = text(msg.sid, 40);
  if (new URL(sender.url).searchParams.get("sid") !== sid) return;
  answer(sid, msg.ok === true, "the person clicked Deny");
  reply(true);
});

// Closing the prompt without clicking is a no.
chrome.windows.onRemoved.addListener((windowId) => {
  for (const [sid, s] of sessions) {
    if (s.consent && s.consent.windowId === windowId) {
      s.consent.windowId = null;
      answer(sid, false, "the person closed the prompt");
    }
  }
});

// ---- requests ------------------------------------------------------------------------------------------------

async function onRequest(msg) {
  const sid = text(msg.sid, 40);
  const id = msg.id;
  const method = typeof msg.m === "string" ? msg.m : "";
  const p = msg.p && typeof msg.p === "object" ? msg.p : {};
  const fail = (why) => send({ t: "res", sid, id, e: text(why && why.message ? why.message : why, 500) });
  const ok = (r) => {
    if (!send({ t: "res", sid, id, r: r === undefined ? {} : r })) fail("the answer was too big to send");
  };
  const s = sessions.get(sid);
  if (!s) return fail("no such session");
  if (s.revoked) return fail("the person stopped nexo in this browser (Cancel on the debugging bar); start a new session to ask again");
  if (!s.granted) return fail("not allowed: the person has not allowed nexo to control this browser");
  try {
    if (NEEDS_TAB.has(method) && !(Number.isInteger(p.tab) && tabOwner.get(p.tab) === sid)) {
      return fail("that tab is not one of this session's tabs");
    }
    switch (method) {
      case "tabs.create":
        return ok(await createTab(sid, s));
      case "tabs.list":
        return ok({ tabs: await listTabs(s) });
      case "tabs.activate":
        await chrome.tabs.update(p.tab, { active: true });
        return ok({});
      case "tabs.close":
        await chrome.tabs.remove(p.tab);
        return ok({ tab: p.tab });
      case "window.resize":
        return ok(await resize(p));
      case "cdp":
        return ok(await command(p));
      default:
        return fail("unknown request " + text(method, 40));
    }
  } catch (e) {
    fail(e);
  }
}

async function createTab(sid, s) {
  if (s.tabs.size >= MAX_TABS) throw new Error("this session already has " + MAX_TABS + " tabs open; close some first");
  let tab;
  try {
    tab = await chrome.tabs.create({ url: "about:blank", active: true });
  } catch (e) {
    // No browser window is open: make one.
    const win = await chrome.windows.create({ url: "about:blank", focused: true });
    tab = win.tabs[0];
  }
  own(sid, s, tab.id);
  try {
    await attach(tab.id);
  } catch (e) {
    release(tab.id);
    chrome.tabs.remove(tab.id).catch(() => {});
    throw e;
  }
  groupTab(tab);
  return { tab: tab.id };
}

async function listTabs(s) {
  const list = [];
  for (const id of s.tabs) {
    try {
      const t = await chrome.tabs.get(id);
      list.push({ tab: id, title: text(t.title, 300), url: text(t.url || t.pendingUrl, 2000), active: Boolean(t.active) });
    } catch (e) {
      /* closed meanwhile */
    }
  }
  return list;
}

async function resize(p) {
  const size = (n, lo, hi) => Math.min(hi, Math.max(lo, Number.isFinite(n) ? Math.round(n) : lo));
  const tab = await chrome.tabs.get(p.tab);
  // A maximized or full-screen window ignores a new size, so it is made a normal one first.
  await chrome.windows.update(tab.windowId, { state: "normal" });
  await chrome.windows.update(tab.windowId, { width: size(p.width, 200, 10000), height: size(p.height, 200, 10000) });
  return {};
}

// One DevTools command for one of the session's tabs.
async function command(p) {
  const method = typeof p.method === "string" ? p.method : "";
  const bad = refuse(method, p.params);
  if (bad) throw new Error(bad);
  if (!attached.has(p.tab)) {
    if (lost.has(p.tab)) throw new Error("the debugger let go of this tab (" + lost.get(p.tab) + "); nexo does not attach again on its own");
    await attach(p.tab);
  }
  // A tab in the background does not paint, and a picture of it would never come: show it first.
  if (method === "Page.captureScreenshot" || method === "Page.startScreencast") await show(p.tab);
  const params = p.params && typeof p.params === "object" ? p.params : {};
  const result = await chrome.debugger.sendCommand({ tabId: p.tab }, method, params);
  return result === undefined ? {} : result;
}

function refuse(method, params) {
  const domain = method.split(".")[0];
  if (!method.includes(".") || !DOMAINS.has(domain) || BLOCKED.has(method)) {
    return "nexo does not send " + text(method, 60) + " to the browser";
  }
  if (method === "Page.navigate" && params && typeof params.url === "string" && INTERNAL.test(params.url.trim())) {
    return "nexo does not open the browser's own pages";
  }
  return "";
}

async function show(tab) {
  try {
    const t = await chrome.tabs.get(tab);
    if (!t.active) await chrome.tabs.update(tab, { active: true });
  } catch (e) {
    /* the command will report it */
  }
}

// ---- tabs and the debugger ----------------------------------------------------------------------------------

function own(sid, s, tab) {
  tabOwner.set(tab, sid);
  s.tabs.add(tab);
  lost.delete(tab);
  paint();
}

function attach(tab) {
  if (attached.has(tab)) return Promise.resolve();
  if (!attaching.has(tab)) {
    const run = chrome.debugger.attach({ tabId: tab }, PROTOCOL).then(() => {
      attached.add(tab);
    });
    attaching.set(tab, run);
    const done = () => attaching.delete(tab);
    run.then(done, done);
  }
  return attaching.get(tab);
}

// nexo lets go of a tab (the debugger detaches, the tab stays open for the person).
function release(tab) {
  const sid = tabOwner.get(tab);
  tabOwner.delete(tab);
  const s = sessions.get(sid);
  if (s) s.tabs.delete(tab);
  lost.delete(tab);
  if (attached.delete(tab)) chrome.debugger.detach({ tabId: tab }).catch(() => {});
}

function groupTab(tab) {
  groupChain = groupChain
    .then(async () => {
      const found = await chrome.tabGroups.query({ windowId: tab.windowId, title: GROUP_TITLE });
      if (found.length) {
        await chrome.tabs.group({ tabIds: [tab.id], groupId: found[0].id });
      } else {
        const groupId = await chrome.tabs.group({ tabIds: [tab.id], createProperties: { windowId: tab.windowId } });
        await chrome.tabGroups.update(groupId, { title: GROUP_TITLE, color: "blue" });
      }
    })
    .catch(() => {
      /* a popup window cannot hold a group; the tab is nexo's all the same */
    });
}

// A page of a session opened a window or followed a link to a new tab: it is the same session's tab.
chrome.tabs.onCreated.addListener(async (tab) => {
  const opener = tab.openerTabId;
  const sid = opener === undefined ? undefined : tabOwner.get(opener);
  const s = sid === undefined ? undefined : sessions.get(sid);
  if (!s || !s.granted || s.tabs.size >= MAX_TABS) return;
  own(sid, s, tab.id);
  try {
    await attach(tab.id);
  } catch (e) {
    release(tab.id);
    return;
  }
  groupTab(tab);
  send({ t: "ev", tab: tab.id, m: "Nexo.tabOpened", p: { opener } });
});

chrome.tabs.onRemoved.addListener((tab) => {
  const sid = tabOwner.get(tab);
  if (sid === undefined) return;
  attached.delete(tab);
  release(tab);
  send({ t: "ev", tab, m: "Nexo.tabClosed", p: {} });
  paint();
});

chrome.debugger.onEvent.addListener((source, method, params) => {
  const tab = source.tabId;
  if (tab === undefined || !tabOwner.has(tab) || !FORWARD.has(method)) return;
  send({ t: "ev", tab, m: method, p: scrub(method, params) });
});

chrome.debugger.onDetach.addListener((source, reason) => {
  const tab = source.tabId;
  if (tab === undefined) return;
  attached.delete(tab);
  const sid = tabOwner.get(tab);
  if (sid === undefined) return;
  lost.set(tab, reason);
  send({ t: "ev", tab, m: "Nexo.detached", p: { reason } });
  // Cancel on Chrome's debugging bar is the person saying stop: the whole session ends.
  if (reason === "canceled_by_user") revoke(sid);
});

function revoke(sid) {
  const s = sessions.get(sid);
  if (!s) return;
  s.revoked = true;
  s.granted = false;
  withdrawConsent(sid);
  for (const tab of [...s.tabs]) {
    if (!lost.has(tab)) send({ t: "ev", tab, m: "Nexo.detached", p: { reason: "canceled_by_user" } });
    if (attached.delete(tab)) chrome.debugger.detach({ tabId: tab }).catch(() => {});
    lost.set(tab, "canceled_by_user");
  }
  paint();
}

// Requests and responses keep their address and outcome; headers, bodies and cookies stay in the browser.
function scrub(method, params) {
  if (method === "Network.requestWillBeSent") {
    const r = params.request || {};
    const out = { requestId: params.requestId, type: params.type, wallTime: params.wallTime, request: { url: r.url, method: r.method } };
    if (params.redirectResponse) out.redirectResponse = { status: params.redirectResponse.status };
    return out;
  }
  if (method === "Network.responseReceived") {
    const r = params.response || {};
    return { requestId: params.requestId, type: params.type, response: { url: r.url, status: r.status, mimeType: r.mimeType } };
  }
  return params;
}

// ---- what the person sees ------------------------------------------------------------------------------------

function paint() {
  const all = [...sessions.values()];
  const asking = all.some((s) => s.consent);
  const live = all.some((s) => s.granted);
  const label = asking ? "?" : live ? "ON" : "";
  chrome.action.setBadgeText({ text: label }).catch(() => {});
  chrome.action.setBadgeBackgroundColor({ color: asking ? "#d97706" : "#2563eb" }).catch(() => {});
  const title = asking ? "nexo is asking to control this browser" : live ? "nexo is controlling tabs in the nexo group" : "nexo browser (idle)";
  chrome.action.setTitle({ title }).catch(() => {});
}

// ---- start up, keep alive ---------------------------------------------------------------------------------------

// A debugging session of a previous life of this worker would keep its bar on screen: let go of everything.
async function sweep() {
  try {
    const targets = await chrome.debugger.getTargets();
    await Promise.all(
      targets.filter((t) => t.attached && t.tabId !== undefined && !attached.has(t.tabId)).map((t) => chrome.debugger.detach({ tabId: t.tabId }).catch(() => {})),
    );
    const prompts = await chrome.tabs.query({ url: CONSENT_URL + "*" });
    await Promise.all(prompts.map((t) => chrome.windows.remove(t.windowId).catch(() => {})));
  } catch (e) {
    /* nothing to sweep */
  }
}

chrome.alarms.create("nexo-keepalive", { periodInMinutes: 1 });
chrome.alarms.onAlarm.addListener((a) => {
  if (a.name !== "nexo-keepalive") return;
  if (port) ping();
  else connect();
});
chrome.runtime.onStartup.addListener(connect);
chrome.runtime.onInstalled.addListener(connect);
// The toolbar button is a manual "connect now" for the moments the host was not there yet.
chrome.action.onClicked.addListener(() => {
  reconnectMs = 1000;
  connect();
});

sweep().finally(connect);
