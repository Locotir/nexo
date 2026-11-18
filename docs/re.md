# nexo RE: the reverse-engineering kit

nexo RE reads and debugs compiled programs. `nexo bin` is static analysis: it reads a file and never runs it. `nexo dbg` is live debugging of a program it starts or attaches to. `nexo bin emu` runs one function in an emulator, `nexo bin patch` changes a copy, and an MCP server hands all of it to other agents.

`nexo re` prints the map of every op and command. `nexo re <word> ...` runs a `nexo bin` op or a `nexo dbg` command (when both have the word, `nexo bin` wins; write `nexo dbg <word>` for the debugger's). Details for one op: `nexo bin <op> --help`; for the debugger: `nexo dbg help`.

## `nexo bin` ops

`S` = saves names, notes or settings in nexo's data folder for that file. `W` = writes a file you name.

| Op | What it answers |
| --- | --- |
| `overview` | The map of a file in one call: main, imports by capability, top strings and functions, next commands |
| `info` | Format, entry, sections, counts, hardening |
| `syms` | Needed libraries, imports (with GOT/IAT slot), exports, symbols, sections, relocations (`--kind`) |
| `funcs` | Functions with size, callers, callees (`--sort addr\|size\|callers\|callees`, `--min-size`) |
| `fn` | Disassembly of the function at an address or name (`--bytes`, `--to ADDR`, `--code`) |
| `decomp` | Pseudo-C of one or more functions (`--addr`); x86-64 |
| `calls` | Who calls a function and what it calls |
| `tree` | Callers (`--up`) or callees to `--depth` levels (`--ptrs`) |
| `path` | Shortest call chains from one function to another (`--routes`, `--ptrs`) |
| `cfg` | Basic blocks, edges, loops, dominators of one function |
| `xref` | Who calls, jumps to, reads, writes or points at an address or name (`--to`, `--str TEXT`, `--i`) |
| `slice` | Backward slice: what produced a value read at an address (`--reg`, `--arg`, `--mem`, `--forward`) |
| `strings` | ASCII and UTF-16 strings with references (`--sort addr\|refs`) |
| `bytes` | A range as a typed table (`--as u32`, `ptr`, `str`, `struct`, DWARF/PDB types) |
| `find` | Hex bytes (`??` wildcard), `--imm N`, `--str TEXT [--wide]`, `--asm REGEX`, `--seq "mov eax, *; call *"`, `--in RANGE`, `--limit` |
| `crypto` | AES, SHA, MD5, CRC-32, ChaCha, base64 constants and the functions that use them. `S` with `--save` |
| `managed` | JVM `.class`/`.jar` and .NET assemblies: types, methods, strings (`--strings`), call sites (`--callers`) |
| `diff` | Two builds, function by function; one function side by side (`--all`) |
| `emu` | Run a function in an emulator: return value, memory written, library calls, trace |
| `asm` | Assemble Intel x86 (`--at`, `--bits 64\|32`) or AArch64 (`--arch arm64`, 4-aligned `--at`); prints bytes and disassembly |
| `patch` | Patch bytes in a copy (`--out`) or in place (`--in-place`). `W` |
| `name` | Name an address; stays with the file (`--clear`). `S` |
| `note` | One-line note at an address (`--clear`). `S` |
| `names` | List the names and notes the file has |
| `proto` | Give a function a C prototype so decomp types its arguments (`--clear`). `S` |
| `struct` | Define a struct for decomp, fields `offset:type:name` (`--clear`). `S` |
| `var` | Rename a variable of one decompiled function (`--clear`). `S` |
| `sigs` | Build library signatures from `libc.a`, the Rust std, etc. so stripped code gets names. `S`, or `W` with `--out` |

## Shared flags

Every op takes these (`Common` in `bin_cmd/args.rs`):

| Flag | Meaning |
| --- | --- |
| `--lines N\|all` | Lines in the answer (N at least 10; default 120). A cut answer ends with the command for the next piece |
| `--from ADDR` | Start at this address or line position (the cut line prints the value) |
| `--grep REGEX` | Keep rows that match, then page them; the first line says how many matched |
| `--json` | Print JSON, same bound |
| `--fields A,B,C` | With `--json`: keep only these keys per row; a wrong key lists the valid ones |
| `--timing` | Print build and total time on stderr |
| `--arch ARCH` | Fat Mach-O slice: `x86-64` or `arm64`. With `--raw`: the code's machine: `x86-64`, `x86-32` or `arm64` |
| `--raw`, `--base ADDR` | The file is bare code (dump, shellcode, unpacked stub) loaded at `--base` (default 0) |
| `--module NAME`, `--live` | Read a module of a running `nexo dbg` session, with live addresses; `--live` takes the bytes from memory. Write `-` for FILE |

Not every op takes `--module`: `diff`, `asm`, `sigs`, `managed`, `patch` and `crypto --save` refuse it.

## Workflows

First look at an unknown binary:

```
nexo bin overview ./a.out
nexo bin strings ./a.out --grep 'password|license' --sort refs
nexo bin xref ./a.out --str 'Invalid key' --i
nexo bin decomp ./a.out check_key
nexo bin tree ./a.out check_key --up
nexo bin note ./a.out check_key 'compares serial, returns 0 on match'
```

Find a check and patch a copy (the original is never touched; `--in-place` keeps `a.out.orig`):

```
nexo bin find ./a.out --asm 'cmp eax, 0x539'
nexo bin fn ./a.out check_key --bytes
nexo bin patch ./a.out --out ./patched --fn check_key+0x14 --old '75 05' --nop 2 --verify
nexo bin patch ./a.out --out ./patched --patches fixes.txt
```

A patches file has one line per patch: `ADDR OLD_HEX NEW_HEX`, `ADDR NEW_HEX` or `ADDR nop N`; `#` starts a comment. All are checked first, then all applied or none. Use `--at ADDR` or `--fn NAME+OFF` for the place, and `--bytes HEX`, `--asm 'mov eax, 7; ret'` or `--nop N` for the new code. A Mach-O copy that was signed (or is arm64) is signed ad hoc again.

Emulate one function (x86-64, never runs the program):

```
nexo bin emu ./a.out check_key str:ABCD-1234
nexo bin emu ./a.out main --stdin 'hello' --max-calls 50
nexo bin emu ./a.out hash buf:32 str:abc --stub rand=4 --show 0x404000:32
nexo bin emu ./a.out 0x401130 --to 0x401190 --reg rbx=5 --break 0x401160 --hook 0x401170=print
```

Arguments are `42`, `-1`, `0x401000`, `str:TEXT`, `hex:DE AD` or `buf:SIZE`. Also: `--abi sysv|ms`, `--mem ADDR=VALUE`, `--steps`, `--secs`, `--trace`, `--dump`, `--stack`, `--clock`, `--seed`. Linux system calls run in the emulator with fixed answers (`read` from `--stdin`, fixed clock, seeded random); one it does not know stops the run with its number and name. FMA3 instructions (`-march=x86-64-v3` code) are supported. `--hook` takes `ADDR=print`, `ADDR=dump:REG:LEN` or `ADDR=set:REG=VALUE`. On arm64 the integer instructions run (registers `x0`..`x30`, `sp`, `--reg x1=5`), imports go through `--stub` and the same library stubs with AAPCS64 arguments, and `svc`, SIMD, floating point and atomics stop the run by name; `--hook` and `--abi` are x86 only.

Attach to a running process (Linux, macOS and Windows; processes of yours):

```
nexo dbg run ./a.out arg1          # or: nexo dbg attach 1234
nexo dbg break check_key
nexo dbg cont
nexo dbg regs
nexo dbg bt
nexo dbg do "break strcmp; cont; bt"
nexo dbg modules
nexo bin decomp - check_key --module a.out
nexo bin fn - 0x7ffff7e00000 --module libc.so.6 --live
nexo dbg end
```

`nexo dbg` keeps the program alive in a small server between calls; `-s ID` picks a session when several are live, and `--lines`, `--from`, `--grep` and `--json` work as in `nexo bin`. Other commands: `step`, `next`, `finish` (prints the returned value and its type, or the raw registers without debug info), `until`, `threads` (on Linux each thread's name, state and system call; `threads --bt N` adds its backtrace, N frames at most), `mem`, `print` (Rust `String`/`Vec`/`&str`/`Option`/`Box`, libstdc++ `string`/`vector`/`unique_ptr` and Go `string`/slices read as values from DWARF; `--as raw` shows the raw struct), `args`, `locals`, `watch`, `catch` (signals, `load`, and `throw|panic|abort` as breakpoints on `__cxa_throw`, `rust_begin_unwind`, `abort`), `strace`, `ltrace`, `hook`, `heap` (`heap on` follows malloc, calloc, realloc and free: a free of a pointer that is not live stops the program and names where the block was allocated and first freed; `heap` and `heap leaks` list the live blocks by allocation site; every call stops the process, so the program runs much slower), `cover`, `find`, `dump`, `core`, `connect`, `serve --gdb PORT`. `stops compact` makes each stop one line (why, where, the `display` values; a crash stays full) and `stops full` undoes it; `signal SIG|0` continues delivering a signal (Linux). `run --repeat N --until crash|exit|output~TEXT ./a.out` starts the program from scratch up to N times (at most 1000) and stops, session live, at the first run that crashes, exits non-zero or prints TEXT, or hits a `--break`, with a tally of the earlier runs. `record [N]` keeps the last N instructions as they run (Linux; slow) and `back` looks at them (`--reg REG`, `--mem ADDR`); it does not undo. `write`, `setreg`, `call` and `stdin` change the program. `nexo dbg help` lists every command.

Batch script:

```
nexo bin --script ops.txt --keep-going
printf 'info ./a.out; funcs ./a.out --grep main\n' | nexo bin --script -
```

Ops are one per line or separated by `;`, `#` lines are comments. Each answer comes under a `== op ==` line. The first error stops the rest unless `--keep-going`; the exit code is the worst one.

## What reads arm64 and x86-32

| Code | Works | Refused |
| --- | --- | --- |
| x86-64 | Every op | |
| arm64 (AArch64) | `info`, `overview`, `syms`, `funcs`, `fn`, `decomp` (no jump tables yet), `xref`, `calls`, `tree`, `path`, `strings`, `bytes`, `find` (bytes, `--imm`, `--str`, `--asm`, `--seq`), `cfg`, `diff`, `slice` (both directions), `emu` (integer code; `svc`, SIMD and floating point stop by name), `patch --nop`, `patch --asm` | |
| x86-32 | `info`, `overview`, `funcs`, `fn`, `xref`, `calls`, `strings`, `bytes`, `find` | `decomp`, `emu`, `cfg`, `slice`, `diff`, nop and asm patches |

The refused ops say so instead of printing a wrong answer. A bare x86-32 or arm64 dump: `--raw --arch x86-32` or `--raw --arch arm64`.

## MCP: nexo for other agents

```
nexo re --mcp                    # read-only (also: nexo bin --mcp)
nexo re --mcp --allow-changes
```

The server speaks MCP over stdio. It offers one `bin_<op>` tool per `nexo bin` op, `bin_batch` (several ops in one call), and `dbg_start`, `dbg_command`, `dbg_stop` for a debugger session that stays alive between calls. Each result is the text `nexo bin` or `nexo dbg` prints.

Read-only by default. Without `--allow-changes` these are refused: ops marked `S` or `W` above, `dbg_start` kinds `run`, `attach` and `connect` (only `core`, which reads a core file or minidump, works), and `dbg_command` words that change state (`write`, `setreg`, `call`, `name`, `note`, `serve`, `calls`, `stdin`, `watch`, hooks that set values). Attach is always limited to your own processes.
