# nexo api

`nexo api analyze HAR --save MAP` reads a capture and writes a map of the API behind a site; `describe` lists it;
`call` sends one request. This page covers the one flag that needs a browser.

## `call --via-browser [=ID]`

```
nexo api call map.json "GET /v1/items" --via-browser
nexo api call map.json "GET /v1/items" --via-browser=ID --dry-run
```

The request goes out of your own browser, where you are already signed in. nexo connects through its installed
extension automatically (authenticated local relay sessions do not show an Allow dialog), opens a tab on the API's
origin if none of the tabs it knows is there, and runs a `fetch` in that page
with `credentials: "include"`, so the cookies of the session ride along. Only the status, the headers (never
`set-cookie`) and the body come back, with the capture's redaction applied, cut at the usual read cap. The output and
the exit codes are those of a normal call. The tab nexo opened is closed again.

Header tokens (bearer, API-key header) are not read by nexo. The map keeps a fingerprint of each one (the first 8
bytes of its SHA-256, no value). The page script hashes every `localStorage` and `sessionStorage` value, and every
string up to depth 2 of the JSON stored there, with `crypto.subtle`, and sets the header from the one that matches.
The token never leaves the page.

Refused before anything is sent:

- the page is not on the map's origin (a map holds no CORS information, so there is no exception);
- a needed token matches nothing in the page: the message names it and says to sign in in that browser first (a token
  that rotated has a new fingerprint: sign in, or capture again);
- `--login`, `--auth`, `--reveal`, a secret param, or a secret field in the body;
- endpoints that need basic auth, a key in the query, or a CSRF header (not supported);
- a map saved by an older nexo has no fingerprints: run `analyze` again.

Limits: the extension only lets nexo see the tabs it opened, so it always works in a tab of its own. The site's own
scripts run in that page too. Redirects are not followed (a page cannot tell where they lead): a redirect is reported
as a lost session. In the agent's `api` tool the flag is gated like `--login`: it needs `--allow-changes`, and
`--dry-run` always works.
