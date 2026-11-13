# Driving your own Chrome: manual test

The automated tests never touch a real person's browser. They use a scripted extension end
(`nexo-browser/tests/relay.rs`, `broadcast.rs`) and, on request, the real extension in a headless Chrome the test starts
itself with a throwaway profile (`nexo-browser/tests/real_extension.rs`, `#[ignore]`). This page is the short check a
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
3. A small window opens in the browser: "Allow nexo to control this browser?" with Deny focused. Waiting about two
   minutes without answering counts as Deny. Click Allow.
4. A blue "nexo" tab group appears with one tab, the toolbar badge reads ON, and Chrome shows its "started debugging this
   browser" bar. Ask the
   model to open a page, fill a form, take a screenshot. Only tabs nexo opened are visible to it; your other tabs are
   neither listed nor touched.
5. Click Cancel on the debugging bar. The session is revoked at once: the next action says the connection ended and
   starts nothing. (Ask again with `select_browser` to be prompted again.)
6. `close` releases the browser and leaves the tabs open for you.

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

## The real-extension test

```text
cargo build -p nexo-cli
cargo test -p nexo-browser --test real_extension -- --ignored --nocapture
```

`NEXO_HOST_BIN` names the host program (default: `nexo` or `nexo-browser-host` beside the test binary). It needs a
Chrome that accepts `--enable-unsafe-extension-debugging` with `Extensions.loadUnpacked` (Chrome for Testing, Chromium,
or branded Chrome 137+ with that flag). The test confirms that a script's `.click()` on Allow is ignored and only a
real mouse click lets a session in. It starts its own Chrome in a temporary folder and kills it at the end.
