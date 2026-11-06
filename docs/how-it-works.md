# How the curator works


nexo keeps the whole conversation; the curator decides the **view**, what the next model call is sent.
Each message is processed once, when it arrives, and nothing is recomputed per call. The code is in
`nexo/crates/nexo-curator` (`engine.rs` the view, `goal.rs` the threads, `ledger.rs` the ledger,
`mechanical.rs` the squeeze), and its learned weights ship in `nexo-curator/data/model.json`.

- **Squeeze at birth** (`birth=squeeze`). The output of a process (a command, an unknown tool) is squeezed
  mechanically the moment it arrives, so the squeeze never breaks the cache later: ANSI codes and progress
  bars removed, identical or same-pattern lines and repeated blocks collapsed into a count, path listings
  grouped by directory and grep results by file (both lossless), very long lines cut. What the read and
  search tools return (a file, a listing, matches) is the material itself and stays as it came: a repeated
  line there is content (an unrolled loop, a row of a table), and in a measured run folded repeats in an
  assembly file cost the answers that depended on them. On real output: `find` −60%, `grep -rn` −45%,
  `ls -la /usr/bin` −94%, `dpkg -l` −90%, `ps aux` −26%.
- **Threads and cards.** A turn is a request of the person and the agent's work on it. A new request
  joins the thread of the earlier turn it most likely draws on, `p(n → j) = σ(f)` from shared words and
  names, recency and the request's length, if that reaches `link` (0.5); otherwise it opens a thread. The
  view is the anchor (the first message), the active thread's turns, and one frozen card per other thread
  (what was asked, done and changed). Threads only grow at the end, so each keeps its cached prefix.
- **The adaptive floor (EOQ cap).** A rewrite sends the `K` tokens that stay anew; between rewrites the
  view grows by `g` tokens per call and each call re-reads it at the cached price `ρ`. Rewriting after `Q`
  tokens costs, per call, `ρ·(head + K + Q/2) + (1 − ρ)·K·g/Q`, least at `Q* = √(2(1 − ρ)K g/ρ)`, the
  economic order quantity. With `eoq=K` the cap is `K + Q*`, from the growth measured in the session, and
  a rewrite over the cap goes down to `floor × cap` with the floor set to `K / cap`: a fast-growing
  session gets a higher cap and a lower floor, so the view still lands at `K` after a rewrite.
- **Batched cuts.** Under the cap, paragraphs leave by rent or buy: a cut is made when what it saves in
  re-reading pays for sending the rest again uncached. The check runs only when it is due (every 4,000
  new tokens or every 4 steps, `check` and `every`), so cuts come in batches instead of breaking the
  cache a little at every call.
- **The anchor.** After a rewrite the anchor holds, in `[curator]` blocks, what left the view, each block
  within its budget in tokens (0 turns it off):
  - **rules** (`rules=2000`): the person's standing instructions, whole, from lines with cues such as
    never, always, don't, must, from now on, remember, nunca, siempre, prohibido, recuerda, a partir de
    ahora, and the lines the model restates as `Rule: …`. They hold for the whole session and are never
    dropped: when the anchor is short of room the ledger shrinks instead. A correction typed once in the
    chat keeps holding long after its message left, without a system prompt or a file;
  - **notes** (`notes=3000`): the agent's own `Finding:` and `Plan:` lines (every finding once, the
    latest plan), written in its text or passed to the `note` tool, whose call is the note itself (a model
    that only calls tools answers a tool); the directives ask for them, so a settled fact is not worked
    out again. With `nudge=N` (off by default: a request for notes made early fixed wrong readings in
    measured runs) the curator does not rely on the model obeying: after N steps without a note it
    appends one request for the notes to the end of the last tool result (a `note` call when the host
    offers that tool, lines of the reply when it does not; never inside the cached prefix, at most once
    per N steps, only the latest request stays in view and it leaves once a note answers it). The request waits while the agent reads a file in parts (a read with a range), up to
    twice the count: with `n` the setting, it comes when `since_note ≥ n`, `since_nudge ≥ n` and
    (not mid-read or `since_note ≥ 2n`);
  - **pins** (`pin=4000`): the files the person's requests name, whole, as last read, and the files the
    agent itself wrote, whole as last written (up to 1,500 tokens each, head and tail kept), newest
    first, scratch files dropped first; a later edit marks a written file "re-read before trusting", a
    large edit or a failed write drops it, a new write replaces it;
  - **the ledger** (`ledger=6000`): what the blocks above leave of 80% of the view's target goes to the
    lines of the moved-out messages whose names (paths, identifiers, values, hosts) are most likely to be
    taken again, chosen greedily by worth per token; hexdumps and bare data stay out, and a line the
    person or the agent wrote weighs twice a tool's output;
  - **the map**: the requests that moved out, each with the agent's closing reply, and the files changed
    and commands run in them.
- **Pockets (off by default).** With `pockets=1` the curator takes the provider to keep more than the
  last request: the last view sent on each topic (its pocket) lives `life` seconds. Back on a topic whose
  pocket lives, the switch costs nothing, and only a pocket that is gone counts as a miss. Nothing more
  is cut for it: the setting for long work that needs all it holds and is taken up again later. How long
  the provider keeps a view is also learned from what it says it read from its cache: a pocket it no
  longer gave, older than a minute, lowers the lifetime, and one it gave older than that raises it back.
  With `pockets=2`, a view whose pocket is gone is read again whole anyway, so it is cut down to the cap's
  floor then, for free, and a cut past where the cache stops is priced as free. On the recorded sessions
  replayed with a request every 30 s and a 2 h pause every 40, input cost falls up to 14 %, but up to
  2.8 times as many words the agent used later had left the view. `ttl` and `cold` are not settings.
  The curator writes nothing to disk: what left the view comes back by the agent reading the file or
  running the command again.
- **Directives (off by default).** With `directives=1` the curator appends its efficiency directives to the
  system prompt, identical on every request so they stay cached; `nexo prompt` shows them. The default view
  is pure: the model gets no order the whole history would not give it.

The default spec (`D_SPEC` in the crate), with a default for every value; a test keeps this copy equal
to it:

```
threshold=0.02;head=turn;recall=1;link=0.5;rule=rent;birth=squeeze;eoq=16000;ledger=6000;notes=3000;rules=2000;pin=4000;protect=12;directives=0;nudge=0;close=2;ahead=0.25;margin=2.25;dedup=3;live=1;lead=1;aim=1;follow=1;brief=1;digest=1;hold=1;loops=0;asked=3;fresh=1;still=32000;sup=1;named=16000;mission=1;apart=8000;check=4000;every=4;ratio=0.1;least=0.0005;floor=0.6;cap=0;cut=age;cards=8;settle=0;topic=0;noted=0;sticky=0;recite=0;task=0;reruns=0;stall=0;stale=1;pockets=0;life=3600
```

| setting | default | what it sets |
|---|---|---|
| `eoq=K` | `16000` | the view after a rewrite, in tokens; the cap is `K + Q*` |
| `ledger=N` | `6000` | the ledger, at most what the blocks leave of 80% of the view's target |
| `rules=N` | `2000` | the person's standing instructions and `Rule:` lines, whole |
| `notes=N` | `3000` | the agent's `Finding:` and `Plan:` lines |
| `pin=N` | `4000` | the files the request names, as last read, and the files the agent wrote, as last written |
| `nudge=N` | `0` | steps without a note (a `Finding:`/`Plan:` line or a `note` call) before the curator asks for one (0 turns it off) |
| `close=N` | `2` | an answered request keeps only the request and its reply; the calls made for it leave by the rent rule |
| `margin=M` | `2.25` | a cut (idle steps, unused outputs, the cap) is made only when what it saves is `M` times what rewriting the view behind it costs |
| `named=1` | on | a step that read a file the latest request names stays |
| `mission=1` | on | a step that read a file the thread's first message lists stays until a request of the thread names that file, once a later request took up another file of that list |
| `live=1` | on | outputs of the open request that no later message drew on go idle and leave when that pays |
| `digest=1`, `hold=1` | on | what the agent has not yet written about is not idle and does not count toward the cap |
| `dedup=3` | `3` | a run of 3+ lines already whole in the view goes as a pointer to it (`asked=3`: not when asked again, unless the same call gave the very same output: then one line says nothing changed) |
| `loops=0` | off | folding a run of identical steps (first and latest stay) sends a view the model already saw, so at a fixed temperature it takes the same step again; off by default |
| `fresh=1` | `1` | the output just asked for goes whole; its repeated runs go as pointers from the next request on (a pointer in a fresh output sends the agent looking for the lines) |
| `still=32000` | `32000` | the latest outputs the agent has not written about yet (no text, no note since), up to this many tokens, leave only as an exact copy of a later output, never on a guess, cap or not; older ones past the budget may go on a guess, so the view stays bounded when the agent never writes |
| `stale=1` | on | past the cap, a read of a file that a later edit or write of it (one that did not fail) changed leaves with the exact copies, before any guess; its note names the file and the message of the edit, and says to read the file again (or `recall`, when the host offers it); decided only at a cut, so the view between cuts stays as sent |
| `pockets=0` | off | 1: the last view sent on each topic stays in the provider's cache for `life` seconds; 2: also cut a view whose pocket is gone (see Pockets above) |
| `life=S` | `3600` | the most seconds a pocket is taken to live; 0: only the last view, as a local server with one slot |
| `directives=N` | `0` | 1 appends the efficiency directives to the system prompt |
| `protect=M` | `12` | the last `M` messages never leave the view |

A spec is `key=value` pairs separated by `;` (a value may hold a comma: `birth=squeeze,telegraph`), laid
over the default; every number is 0 or more. A pair the curator cannot run (an unknown name, a value of
the wrong kind, or a setting it does not support, such as `ttl=60`) is left out, the rest still applies,
and a warning names it.


## The ledger, in more detail

The ledger is where most of the moved-out knowledge survives, so what it keeps matters:

- **Worth per token.** Lines are scored by the names they carry (paths, identifiers, values, hosts) and
  how likely each name is to be taken again; the greedy pick fills the budget by worth per token.
- **One form, four rows.** When a message holds many rows of the same shape (a table, a listing, the
  monsters of a game, the entries of a directory), the ledger keeps the first four and marks how many more
  there were (`… +N rows of this form`). A signed number is still a number, so `L -3 K 19` and
  `L 16 K 200` are one form.
- **Noise stays out.** Hexdumps, bare data and what a binary says about itself (section names, format
  strings, library symbols, disassembly labels, pointers printed in decimal) are not worth their tokens
  and never enter.
- **A file's body weighs a third.** The lines of a file the agent read or wrote are on disk, one read
  away, and the pins hold what it wrote; the ledger wants the file's name and shape, not its text.
- **People weigh twice.** A line the person or the agent wrote weighs twice a tool's output.

On a 100-step `realm` recording this took the ledger from 588 lines to 55 without losing the commands,
paths, breakpoints and data rows the later steps used.

## Reading the screen: the mark on each tool card

Every tool card in the transcript ends its first line with what the last model call did with that output:

| Mark | Meaning |
|---|---|
| `● cached` | Sent whole, in the unchanged start of the request: the provider's prompt cache serves it at about a tenth of the price. |
| `● full price` | Sent whole, after the point where the request changed: paid at the normal input price. |
| `◐ trimmed` | Sent shortened: errors, results and the first and last lines kept, with a note naming what was cut. |
| `○ set aside` | Left out of this call. The model did not see it this time, but it stays in the history under its name: nothing is lost. |
| `↺ recalled` | It had been set aside and came back, whole or in part, because the model asked for it (`recall`) or the work returned to it. |

Set aside is the curator's main saving: the view stays small while the history keeps everything. Recalled is
the proof that leaving something out is reversible.
