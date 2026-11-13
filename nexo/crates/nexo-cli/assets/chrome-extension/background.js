// nexo browser: service worker. Relays messages between the nexo native host (Rust, stdio) and the tabs.
// Protocol: JSON objects with a "type" and an optional numeric "id". The host answers "ping" with "pong",
// and turns "navigate" / "title" requests into commands that run here.

const HOST = "dev.nexo.browser";
let port = null;

function send(msg) {
  if (port) port.postMessage(msg);
}

function connect() {
  if (port) return port;
  port = chrome.runtime.connectNative(HOST);
  port.onMessage.addListener(onHostMessage);
  port.onDisconnect.addListener(() => {
    const why = chrome.runtime.lastError ? chrome.runtime.lastError.message : "closed";
    console.warn("nexo host disconnected:", why);
    port = null;
  });
  send({ type: "ping" });
  return port;
}

async function activeTab() {
  const [tab] = await chrome.tabs.query({ active: true, lastFocusedWindow: true });
  return tab;
}

async function onHostMessage(msg) {
  try {
    if (msg.type === "pong") {
      console.log("nexo host: pong");
    } else if (msg.type === "navigate") {
      const tab = await activeTab();
      if (!tab) throw new Error("no active tab");
      const done = await chrome.tabs.update(tab.id, { url: msg.url });
      send({ type: "result", id: msg.id, ok: true, tab: done.id, url: msg.url });
    } else if (msg.type === "title") {
      const tab = await activeTab();
      if (!tab) throw new Error("no active tab");
      const answer = await chrome.tabs.sendMessage(tab.id, { type: "title" });
      send({ type: "result", id: msg.id, ok: true, title: answer.title, url: answer.url });
    }
  } catch (e) {
    send({ type: "result", id: msg.id, ok: false, error: String(e && e.message ? e.message : e) });
  }
}

chrome.runtime.onStartup.addListener(connect);
chrome.runtime.onInstalled.addListener(connect);
// Clicking the toolbar button also connects, so one click proves the whole path works.
chrome.action.onClicked.addListener(connect);
connect();
