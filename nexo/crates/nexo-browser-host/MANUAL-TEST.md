# Driving your own Chrome: manual test

The automated tests never touch a real person's browser. They use a scripted extension end
(`nexo-browser/tests/relay.rs`, `broadcast.rs`) and, on request, the real extension in a headless Chrome the test starts
itself with a throwaway profile (`nexo-browser/tests/it/real_extension.rs`, `#[ignore]`). This page is the short check a
person makes once with the Chrome they actually use.

## One time

1. `nexo chrome-setup`. It writes the native-messaging manifest for Chrome, Chromium, Edge and Brave (Linux:
   `NativeMessagingHosts` folders; Windows: `HKCU` keys), creates the token and copies the extension to
   `<nexo data dir>/chrome-extension`.
2. In the browser, open `chrome://extensions`, turn on Developer mode, click Load unpacked and pick that folder.
3. Click the nexo button in the toolbar: it only reconnects to the host (use it if the host was not there yet). Its tooltip
   reads "nexo browser (idle)", with no badge, until a session asks.

## The check

1. Start a nexo session and ask the model to call `browsers`. It lists `headless` (the default) and your browser, with a
   star on the one in use. Nothing has been asked of you yet: no window appeared.
2. Ask it to `select_browser` with your browser's id (or `switch_browser` to ask every running browser at once).
3. The session is granted automatically; no nexo consent window appears. A blue "nexo" tab group appears with one tab,
   the toolbar badge reads ON, and Chrome shows its own "started debugging this browser" bar. Ask the model to open a
   page, fill a form and take a screenshot. Only tabs created by this session are visible to it; your other tabs and
   other sessions' tabs are neither listed nor touched.
4. Click Cancel on Chrome's debugging bar. That session is revoked: its next action fails and it cannot be re-granted.
   Starting a new session can connect automatically again.
5. `close` releases the browser and leaves the tabs open for you.

Automatic access means any authenticated nexo relay session can control its own browser tabs without a separate
per-session confirmation. Chrome's debugging bar remains visible and provides the Cancel control.

## What differs from nexo's own headless Chrome

- `resize` sets the outer window size, not the page viewport.
- `close` leaves the tabs open; nexo only lets go of them.
- JavaScript dialogs (alert, confirm, prompt) are dismissed automatically.
- File inputs cannot be clicked: use `upload`.
- Only tabs nexo opened (or that they opened) are in scope.
- Cookie, storage and download commands are refused, and so are the browser's own pages (`chrome://`, `edge://`, the
  extensions page): the extension answers "nexo does not open the browser's own pages".
- Cancel on the debugging bar, or closing the browser, ends the session.
- Logins, passwords, payments and CAPTCHAs stay with the person: the model is told to stop and ask.

## Offline worker smoke test

`node --test tools/chrome-extension-smoke.cjs` exercises the shipped service worker's automatic grant,
pre-grant request gate, cross-session tab isolation and revocation/no-regrant handlers with stub Chrome APIs.
It does not start Chrome or the native host and is not a substitute for the real-extension test below.

## The real-extension test

```text
CARGO_TARGET_DIR="$HOME/build-cache/nexo-localbuild" nice -n 10 cargo build -j 2 -p nexo-browser-host
CARGO_TARGET_DIR="$HOME/build-cache/nexo-localbuild" nice -n 10 cargo test -j 2 -p nexo-browser --test it real_extension -- --ignored --nocapture
```

`NEXO_HOST_BIN` names the host program (default: `nexo` or `nexo-browser-host` beside the test binary). It needs a
Chrome that accepts `--enable-unsafe-extension-debugging` with `Extensions.loadUnpacked` (Chrome for Testing, Chromium,
or branded Chrome 137+ with that flag). The test checks automatic connection without a prompt, session isolation and revocation. It starts its own Chrome in a temporary folder and kills it at the end.
