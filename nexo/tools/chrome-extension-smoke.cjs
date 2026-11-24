// Run: node --test tools/chrome-extension-smoke.cjs
// Exercises the shipped worker handlers, not a reimplementation. No real Chrome/profile/native host.
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const { test } = require('node:test');

function worker() {
  const event = () => ({ addListener() {} });
  const sent = [];
  const chrome = {
    tabs: { onCreated: event(), onRemoved: event() },
    debugger: { onEvent: event(), onDetach: event() },
    alarms: { create() {}, onAlarm: event() },
    runtime: { onStartup: event(), onInstalled: event() },
    action: {
      onClicked: event(), setBadgeText: async () => {},
      setBadgeBackgroundColor: async () => {}, setTitle: async () => {},
    },
  };
  const ctx = vm.createContext({ chrome, console, out: sent });
  const source = fs.readFileSync(path.join(__dirname, '../crates/nexo-tools/assets/chrome-extension/background.js'), 'utf8');
  // Suppress only startup, which would connect a native host. Fail loudly if its shape changes.
  assert(source.endsWith('sweep().finally(connect);\n'));
  vm.runInContext(source.replace('sweep().finally(connect);', ''), ctx);
  vm.runInContext('port = {postMessage(m) {out.push(m)}};', ctx);
  return { sent, run: code => vm.runInContext(code, ctx) };
}

test('grant is automatic but only existing sessions may receive it', () => {
  const { run, sent } = worker();
  run('askConsent("missing")');
  assert.equal(sent.length, 0);
  run('openSession("s1", "test"); askConsent("s1")');
  assert.equal(sent.length, 1);
  assert.equal(sent[0].t, 'consent_result');
  assert.equal(sent[0].sid, 's1');
  assert.equal(sent[0].ok, true);
});

test('requests need a grant and cannot address another session\'s tabs', async () => {
  const { run, sent } = worker();
  await run('onRequest({sid:"missing",id:1,m:"tabs.list"})');
  assert.match(sent.pop().e, /no such session/);
  run('openSession("s1", "test")');
  await run('onRequest({sid:"s1",id:2,m:"tabs.list"})');
  assert.match(sent.pop().e, /not been granted/);
  run('askConsent("s1"); openSession("s2", "other"); tabOwner.set(99, "s2")');
  assert.equal(sent.pop().ok, true);
  await run('onRequest({sid:"s1",id:3,m:"tabs.activate",p:{tab:99}})');
  assert.match(sent.pop().e, /not one of this session/);
});

test('Cancel revokes access and a subsequent grant cannot undo it', async () => {
  const { run, sent } = worker();
  run('openSession("s1", "test"); askConsent("s1")');
  assert.equal(sent.pop().ok, true);
  run('revoke("s1"); askConsent("s1")');
  assert.equal(sent.pop().ok, false);
  await run('onRequest({sid:"s1",id:4,m:"tabs.list"})');
  assert.match(sent.pop().e, /person stopped/);
  // A fresh session is intentionally a new grant, not a persistent browser-wide ban.
  run('openSession("s2", "new"); askConsent("s2")');
  assert.equal(sent.pop().ok, true);
});
