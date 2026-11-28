# Crate graph

Edges point at what a crate depends on (normal dependencies; dev-dependencies are left out). A crate may use
only crates in the layers above its own; new code must not add an edge that points up, for example from
`nexo-agent` to `nexo-tui`.

| Layer | Crates | Depends on |
| --- | --- | --- |
| Leaves | `nexo-core`, `nexo-http`, `nexo-race`, `nexo-binmagic`, `nexo-relay` | nothing in the workspace |
| Low | `nexo-bin` (binmagic), `nexo-browser-host` (relay), `nexo-providers` (core, http) | leaves |
| Mid | `nexo-dbg` (bin), `nexo-browser` (relay, browser-host), `nexo-supervise` (core, providers) | low |
| Tools | `nexo-tools` | core, binmagic, dbg, browser, relay, http, bin |
| Engine | `nexo-agent` | core, providers, RACE, tools, supervise, binmagic |
| Interface | `nexo-tui` | core, agent, supervise, providers |
| Binary | `nexo-cli`, `nexo-bench` | everything |

Shared pieces that two layers need live in `nexo-core`: the `ModelClient` trait (`model_client`, implemented
for the provider client in `nexo-providers`), the todo list reader (`todo`), the local day (`clock`), and the
readers the rules lean on (`paths::real_path`, `shell::osascript_scripts`). That is what lets `nexo-supervise`
sit below the engine while the loop still uses its `watch` and `policy` modules.

Global state of a sibling crate is set once from `nexo-cli` start-up (`nexo_dbg::set_data_dir`), never from a
tool constructor.
