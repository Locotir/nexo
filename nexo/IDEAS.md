# Ideas: fewer tokens, fewer steps, less spent on the cache

Ranked by what each should save for the work it takes. `FEATURES.md` lists what has tests; the status table below says which of these ideas the code already does.
Effort: S (a day or less), M (a few days), L (a week or more). "Measure" says how to tell it worked; most
can be measured offline on recorded sessions (of several harnesses, nexo among them) before any live run.

### Proposed: account-aware model and effort discovery

Discover capabilities per provider, login type and account. Codex first: verify the authenticated ChatGPT backend `GET /models?client_version=…` with nexo's own session; parse `models[].slug` and `supported_reasoning_levels` separately from the OpenAI API `data` schema. Cache efforts with model IDs, source and freshness; never cache credentials. Label offline/stale and configured-only entries. Verify the actual Responses payload for `max`; do not silently translate it to `xhigh`. Investigate `ultra` delegation separately from reasoning effort. For Claude Code, Grok Build and others, verify each provider's supported discovery mechanism before advertising account availability. Acceptance: schema fixtures, cache round-trip, account switch, expired login, outgoing effort and a fresh authenticated refresh distinguishable from a cached CLI result. No live entitlement check claimed here.

Status as of 2026-11-16, checked against the code. "Shipped" means the behaviour in the idea exists and has a test; "partial" says what exists and what is missing; "open" means nothing in the code does it. Ideas 1 to 14 below keep their original wording, so a "partial" row is the place to read what is left.

| # | Status | What the code does today |
|---|---|---|
| 1 | partial | `postcheck.rs` runs the project's tests or compiler after a change and reports the count and the failing test names (`report`, tests at `postcheck.rs:430-436`). Plain `bash` with `cargo test` or `npm test` returns the whole output; there is no per-tool filter in `bash`. |
| 2 | shipped | `edit` and `write` answer with a unified diff of the change with 3 lines of context, or the whole file when it is short (`edit/show.rs`). They also append `diagnostics:` from the project's compiler check and from a language server that is already running (`edit_check.rs`, `lsp/quick.rs`; test `the_edit_tool_carries_it_and_the_switch_turns_it_off`). `[tools] edit_diagnostics` turns it off. |
| 3 | partial | `read` takes `paths` (up to 20 files) and `symbol` (exact span, ambiguity reported; test `rust_symbol_lookup_returns_exact_span_and_reports_ambiguity` in `read/tests.rs`). `read` of a large file without a range does not answer with an outline first. |
| 4 | partial | `agent.attach_named_files` (on by default) puts up to 3 files the message names, each of 24 KB or less, whole in the request (`turn/start.rs` `named_files`; tests in `tests/agent_loop/attached_files.rs`). The outline form and the named-lines form are not done. `ask_hint.rs` is a different feature: it names the tools of a compiled program a message mentions. |
| 5 | open | No `explore_model` setting exists. Sub-agent profiles (`subagent/profile.rs`) take a `model` of their own, which is the nearest thing. |
| 6 | shipped | `turn/prefix.rs` keeps the parts of the prompt that could change by themselves stable. The test `tool_round_and_next_turn_preserve_prefix_and_account_for_cache_usage` (`tests/agent_loop/request_prefix.rs`) asserts that the system text, the tools and every earlier message are identical across consecutive requests. Cache misses are counted after the fact from the session files (`InsightKind::CacheMisses` in `session_scan`, shown in the Usage tab); there is no live debug counter. |
| 7 | shipped | `turn/cache_ttl.rs` chooses the 1-hour cache write when the session's own past pauses exceed the 5-minute lifetime and the write costs less than a re-write (`Pricing::long_cache_pays`; `anthropic/request.rs:59`). A one-token keep-warm request does not exist. |
| 8 | partial | `postcheck.rs` answers "same output as the check above" for an unchanged check. `repeat.rs` adds one line when the same failure comes back (`st.repeats.round`, `turn/round.rs`). Output of other `bash`, `grep` or `glob` calls that repeats an earlier one is sent whole. |
| 9 | partial | The briefing has a token budget with the ledger limited to 40% of it (`brief.rs`). Sizing it by what the parent read or by the length of the task prompt is not done. |
| 10 | open | Compaction (`turn/compaction.rs`) still asks the model for a summary. No ledger-only compaction was found. |
| 11 | shipped | `grep` keeps `max_per_file` (default 20) and `max_results` (default 200) and ends with "N more matches in M files; narrow the pattern or the path" (`grep.rs:20-51`, test near line 720). `glob` grouping by folder was not found. |
| 12 | open | No report contract with cited `path:line` was found in the sub-agent profiles, and no `subagent_report` setting. |
| 13 | partial | Only the asks after a reply cut at `agent.reasoning_budget` use the lowest effort a model lists (`calm_level`, `turn/round.rs:736`). Lowering effort for read-only steps is not done. |
| 14 | partial | The briefing ledger exists (`brief/ledger.rs`) and is rebuilt from the history for each briefing. It is not kept in the session file and there is no `/ledger` command. |

| # | Idea | Why it saves | Effort | Measure |
|---|---|---|---|---|
| 1 | **Shape test and build output at the source.** `bash` recognises `cargo test`, `go test`, `npm test`, `tsc`, `cargo build` and keeps the failing tests, panics, the first error of each kind and the summary line; passing lines become a count. The whole output stays on disk, reachable by `jobs output`. | Test and build logs are the largest tool outputs in coding sessions and every token of them is re-read from the cache on every later call. | S | Tokens of `bash` outputs per session on recorded sessions, before and after the filter; share of later calls that re-run the same command with `grep`/`tail` (should not rise). |
| 2 | **An edit answers with the lines around it.** `edit` and `write` return the changed hunk with its line numbers and three lines of context, plus `diagnostics` when a language server is on. | The model re-reads a file after editing it to check the change: one call and the file's tokens, per edit. | S | In recorded sessions, the share of edits followed within two calls by a read of the same file (before vs after). |
| 3 | **Read several files, or one symbol.** `read` takes `paths: [...]` (done) and `symbol: "parse_line"` (the item's span from a regex or tree-sitter outline); `read` of a large file without a range answers its outline first. | Reading five files takes five calls when the model does not batch; reading a 600-line file to see one function costs 600 lines. | M | Calls per session that are a single read right after a single read; read tokens per session; follow-up reads of the same file. |
| 4 | **Name the files of the request before the first call.** When the person's message names a path that exists (or a symbol `grep` finds once), nexo puts its outline (or the named lines) in that message, deterministically. | The first step of most turns is reading the file the person just named. | S | Share of turns whose first tool call reads a path named in the request (recorded sessions); steps per turn. |
| 5 | **A cheaper model for `explore` sub-agents.** `[agent] explore_model` (default: the provider's cheapest model with tools), the briefing standing in for what a stronger model would work out. | Searching and reading need little reasoning; output tokens of a large model cost 5-10× input. | S | Cost per `task` and the share of reports the main agent acts on without re-checking, on a replay of `task` prompts with both models. |
| 6 | **Keep the prefix byte-stable, and check it.** A test that builds two consecutive requests and asserts that the system prompt, tools (sorted), MCP tool list and every earlier message are byte-identical; a debug counter of cache misses when the provider reports cached tokens. | One changed byte early in the prompt (a tool listed in another order, a date, a reordered MCP server) makes the whole request be paid at full price. | S | `cache_read / input` per request from the providers' usage reports; number of requests whose cached prefix shrank. |
| 7 | **Pay the cache's lifetime on purpose.** When the turn ends and the person typically answers after the cache's lifetime (5 minutes on most providers), choose between letting it expire, a one-token keep-warm request before it does, or the 1-hour cache where offered (price of the write vs the expected gap, learned from the session's own gaps). | A context of 100k tokens re-written after a pause costs 12.5× what it costs to read it cached. | M | Cache-write tokens per session against the gaps between requests (the recorded timings have the gaps); spend per session on recorded timings. |
| 8 | **Same output, said once.** When a command is run again and its output is the same as an earlier call's, `bash` answers "same output as call N (exit 0)"; the same for `grep` and `glob`. | Re-running a check that did not change re-sends its whole output into a context that already holds it. | S | Share of `bash`/`grep` outputs identical to an earlier output in the same session (recorded sessions); tokens they hold. |
| 9 | **Size the briefing to the task.** Brief only when the parent read or ran something the task bears on, or when the task prompt is short; stop adding excerpts when the next one adds less than a set share of the task's words; nothing when the prompt already names every path the ledger would. | The replay of a recorded session of 26 sub-agents shows briefings filled to the 3k budget every time, and that with long, self-contained prompts most of their value is in the file list. | S | The same replay: files named per briefing token, and briefing tokens × calls (cache cost) per sub-agent, before and after. |
| 10 | **Compaction from the ledger, not from the model.** When the window fills, keep the ledger (requests, rules, files with their state, failing commands, plan) and the last turn verbatim instead of asking the model for a summary; ask the model only if the person asks for a summary. | A compaction is one large model call (the whole context in, a long summary out) and loses the exact paths and line ranges, which then get re-read. | M | Cost of compaction calls; reads of already-read files in the ten calls after a compaction, on recorded sessions with compactions. |
| 11 | **Cap and count search results.** `grep` keeps at most N matches per file and M in all, then "37 more in 5 files (`grep … path` for them)"; `glob` groups by folder past a size. | A broad search can return thousands of lines the model skims for one; the narrower search that follows is then the useful one. | S | Tokens of grep outputs per session (distribution's tail); share of greps followed by a narrower grep. |
| 12 | **A report contract for sub-agents.** The report starts with the answer in five lines and cites `path:line` with the lines it relied on (at most 20), so the main agent does not open the files to check; `[agent] subagent_report` sets the cap (2000 now). | The main agent re-reads what a sub-agent cites to trust it; the citation with its lines saves the read. | S | Reads by the main agent, within three calls of a `task` result, of files the report cites. |
| 13 | **Reasoning effort by step.** Low effort for steps that only call read-only tools after a plan exists; the configured effort for the first step of a turn, edits and the final answer. | Reasoning tokens are paid as output (the most expensive kind) on every step, including the many that only fetch. | M | Output and reasoning tokens per step by kind; success on a live bench at equal settings otherwise. |
| 14 | **One ledger for the session, kept as it goes.** The briefing's ledger computed once per call and kept in the session file, shown by `/ledger`, and used by the curator's view and by compaction alike. | Today it is rebuilt from the whole history for each sub-agent; a kept ledger also lets a resumed session start with it instead of re-reading. | M | Time per briefing on long sessions; reads after `--continue` of files the ledger marks current. |

## Notes on the evidence so far

The sub-agent briefing (README, "The briefing") was replayed on one recorded session with 26 sub-agents: it
names 25% of the files they went on to read (40% when the same work is handed over half-way), against 15%
for the last 3000 tokens of the conversation and 53-63% for the whole conversation; no sub-agent tool call
there would have been answered whole by what its parent held (2%). Ideas 1-4 and 8 act on every session,
not only those that delegate, and are where the recorded sessions spend their tokens.

## Idea: `/panel` as a phone-style board

Status: open as a design. `/panel` already exists as the agents view (`crates/nexo-tui/src/agents_view/`), with satellites for sub-agents (see `FEATURES.md`); the card layout below is not built.

Make `/panel` read like a mobile agent dashboard: one card per live session with its state at a glance
(working, waiting for you, done, blocked), the latest conclusion in one line, a progress mark, and the
requests pinned to it; open a card for its events, todo and mail; message, pause or resume from the card.
Conclusions first, no step-by-step log on the card. To be designed before any code.

## Propuestas de apps para vender (Microsoft Store y App Store)

Datos tomados el 2026-11-18. Fuentes: API pública de apps.microsoft.com (búsqueda, fichas y reseñas, EE. UU.),
`StorePrep/portfolio/SALES.md`, `StorePrep/research/ideas/NICHOS-20261118.md` y webs citadas.
[V] = lo he visto en la fuente. [I] = deducido, falta probarlo.

### Lo que dicen tus ventas (30 días, 278 $ brutos, 25 ventas)

| Qué vende | Qué no vende |
|---|---|
| Herramientas generales y grandes a 20-27 $ con prueba: PDF Pro 120,85 $, Video Pro 72,46 $, CAD Tools 27,16 $, Docs Pro 24,33 $ [V] | Visores de nicho: INDD Viewer, AI File Viewer, Folio. Tienen 8-12 pruebas cada uno y 0 ventas [V] |

Regla que sale de aquí: trabajo que mucha gente busca, líder flojo en la Store, precio de 20-40 $ con prueba.
El tráfico orgánico llega solo si la palabra que busca la gente («iphone backup», «transcribe audio») te pone arriba.

### Ranking

| # | App | Tienda | Precio | Esfuerzo | Por qué gana |
|---|---|---|---|---|---|
| 1 | Gestor de iPhone para Windows, pago único | Microsoft | 39,99 $ | L | La app oficial de Apple tiene 2,2 ★ y la de pago líder pasó a suscripción |
| 2 | Transcriptor local de audio y vídeo | Microsoft | 24,99 $ | M | MacWhisper vende mucho en Mac y no existe en Windows |
| 3 | Foto de pasaporte con precio claro | App Store | 4,99 $ | S | Los líderes cobran 18-20 $ por foto y tienen un 34 % de reseñas malas |
| 4 | Árbol genealógico sin conexión, con póster | Microsoft | 24,99 $ | M | Público fiel durante años; las apps de la Store son flojas |

### 1. Gestor de iPhone para Windows (copia, fotos, mensajes)

Qué hace: copia de seguridad del iPhone sin pedir actualizar iOS, ver la copia por dentro, sacar fotos (HEIC a JPG),
exportar mensajes y WhatsApp a PDF, pasar música. Todo local, sin cuenta, un solo pago.

Datos:
- Apple Devices (la app oficial): **2,2 ★ con 1.952 valoraciones**; de sus 100 últimas reseñas (2026-09-05 a 2026-11-18), **90 son de 1-2 ★** [V].
- Quejas repetidas: «quería hacer una copia, me obligó a actualizar y me bloqueó el iPhone», «no puedo sacar mis archivos», «para pasar una canción me pide otra copia» [V].
- iTunes: 3,1 ★ con 29.000 valoraciones [V].
- Rivales en la Store: Coolmuster 4,0 (6 val.), Dr.Fone 3,1 (79), FoneBackup 0 [V]. **No hay ninguno bueno.**
- iMazing (el líder fuera de la Store) es **solo suscripción desde junio de 2025**: 29,99 $/año un iPhone, 49,99 $/año cinco [V webs de precios]. En r/iMazing piden «licencia para siempre de 40-70 $» [V].
- Las búsquedas «iphone backup», «iphone transfer» e «iphone manager» devuelven Apple Devices arriba [V]: quien busca ya está enfadado.

Cómo:
- libimobiledevice (LGPL-2.1, enlace dinámico) habla con el servicio Apple Mobile Device que instala Apple Devices [I, probar en la VM].
- Reutiliza tu motor PDF para exportar mensajes y tu motor de foto para HEIC.

Riesgos:
- ⚠️ Necesita el driver de Apple instalado; falta comprobar que una app de la Store puede usarlo [I].
- ⚠️ Apple puede cambiar el protocolo con una versión de iOS: hay que mantenerla.
- Legal bajo: son los datos del propio usuario, nunca salen del PC.

### 2. Transcriptor local (audio y vídeo a texto y subtítulos)

Qué hace: arrastras un audio o vídeo, sale el texto con tiempos, quién habla, subtítulos SRT/VTT, Word y PDF.
Sin internet, sin límite de minutos, un pago.

Datos:
- MacWhisper (solo Mac, un desarrollador): unas **300.000 copias a 59-64 €** según una web que lo recoge; un rastreador de Gumroad le estima 595.600 ventas [V web, cifras no oficiales].
- «MacWhisper for Windows» es una búsqueda con artículos propios: no hay versión para Windows [V].
- Líderes de la Store: Transcribe 4,6 ★ (2.087) y Speech to Text 4,7 (1.004) [V]. Son de nube con límite de minutos: «grabé la reunión y al transcribir salió el muro de pago», «me pide reseña antes de probarla» [V].
- Rival real: WizWhisp, offline y gratis, 4,8 ★ con **solo 155 valoraciones** [V]. Su queja: corta el texto en trozos de 5-10 segundos [V].
- Transcriptores de pago en la Store: Quick Transcribe 8,79 $ y Open Transcriber 5,59 $, **0 reseñas** [V].

Cómo:
- whisper.cpp o Parakeet en local, con GPU o NPU si hay.
- Editor con el audio sincronizado, párrafos por persona, exportar con tus motores de Docs y PDF.
- Mejor que WizWhisp en: quién habla, carpeta entera de una vez, editor, párrafos limpios.

Riesgos:
- ⚠️ Modelos pesados (de 150 MB a 1,5 GB): descarga al primer uso.
- Ya lo tenías apuntado (I026, I209, I929) pero sin estudiar; con estos datos merece MARKET.md.

### 3. Foto de pasaporte y carné (iPhone)

Qué hace: foto con el iPhone, recorte por país, fondo blanco, hoja 4x6 para imprimir en una tienda por céntimos.
Precio a la vista desde la primera pantalla, fotos ilimitadas.

Datos (de `NICHOS-20261118.md`):
- 4 líderes con **16.700 a 25.500 valoraciones** cada uno [V iTunes].
- **230 de 677 reseñas de 2026 son de 1-2 ★ (34 %)** [V].
- Cobran **17,99-20 $ por foto**, a menudo al final del proceso; unas 78 quejas de eso y 45 de «gratis que no lo es» [V].

Cómo: detección de cara de Apple (Vision), plantillas por país, motor de foto propio. Unos 15 días según el estudio.

Riesgos: ⚠️ si la foto es rechazada, la culpa cae en la app. No prometer «aceptación garantizada».

### 4. Árbol genealógico sin conexión, con póster gigante

Qué hace: abre y edita archivos GEDCOM, sin internet, e imprime el árbol en varias hojas A4 con marcas de corte (idea I004).

Datos:
- En la Store: My Family Tree 4,1 ★ (200), EZ Genealogy 3,2 (17), Family Tree Builder Pro 2,8 (11) [V].
- Quejas de My Family Tree: se cuelga, «pide internet solo para abrir mi GEDCOM», «la versión vieja era más fácil» [V].
- Fuera de la Store se paga: RootsMagic 39,95 $, Family Tree Maker unos 80 $ [V webs de reseñas].
- Quien hace genealogía usa el mismo programa muchos años: cliente para mucho tiempo [I].

Riesgos: ⚠️ demanda en la Store más pequeña que las tres de arriba; ya había un estudio flojo (I441, 19/30).

### No repetir

Scanbird (impresoras y escáner sin drivers) ya está en marcha en StorePrep: sigue siendo buena, no la dupliques.

### Ideas sueltas de antes

- Marcar ventanas del PC para que no salgan en capturas ni compartir pantalla (flag OS, ej Google Meet).
- Nicho iOS: calculadora tubería fontaneros, examen radioaficionado, calculadora gas instaladores, examen piloto drones, test naturalización alemán, reglas navegación.
- Impresión: póster grande en varias A4 con marcas corte, fotos carnet por país imprimir A4/10x15, etiquetas envío 4x6 a impresora térmica, calendarios imprimibles.
- Visores sin programa original: archivos .ai Illustrator, .indd InDesign, .pub Publisher.
- Oficina sin suscripción: docs, hojas, presentaciones pago único, visor facturas XML.
- Otras: transcripción local sin conexión, alertas auroras/ríos/terremotos, freno gasto IA devs, app oficina IA local.
