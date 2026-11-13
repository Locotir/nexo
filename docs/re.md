# nexo RE: commands

nexo RE is the kit for reading and debugging compiled programs. Start with `nexo re`: it prints one line for each command.

```
nexo re                          the map
nexo re overview ./a.out         read a file: main, imports, strings, what to ask next
nexo re decomp ./a.out main      the same as: nexo bin decomp ./a.out main
nexo re run ./a.out              run it under the debugger, the same as: nexo dbg run ./a.out
```

`nexo bin` reads a file and never runs it. `nexo dbg` runs a program, or attaches to one of yours, and answers one bounded call at a time. When a word exists in both, `nexo re` takes the `nexo bin` one and the map says so; write `nexo dbg <word>` for the other. In a session, the model is pointed to `nexo re` the first time it reads a binary by hand, so the prompt stays the same size.

## Assemble

`nexo bin asm 'start: nop; jmp start' --at 0x401000 --bits 64` assembles Intel x86 syntax with iced-x86 and prints hex bytes and disassembly. Separate instructions with semicolons or newlines; labels use `name:` and branches use rel32 fixups. The front end accepts registers, decimal or `0x` immediates, and memory such as `qword ptr [rbp-8]`; directives and macros are not supported.

## Patch a copy

`nexo bin patch ./a.out --out ./patched.out --fn main+0x4 --asm 'mov eax, 7; ret'` writes a new copy and prints the offset, old/new bytes and disassembly. Use `--at ADDR` instead of `--fn NAME+OFF`, and `--bytes '90 90'` or `--nop 2` instead of `--asm`. The destination must not exist, and the replacement must fit entirely within one section's file-backed bytes. The input stays unchanged.

## Use it from another agent (MCP)

`nexo re --mcp` (also `nexo bin --mcp`) serves the kit to other harnesses as an MCP server on stdio: one `bin_<op>` tool for each `nexo bin` op (generated from its definition), plus `dbg_start`, `dbg_command` and `dbg_stop` for a session that stays alive between calls; each result is the text `nexo bin` or `nexo dbg` prints.
Saved names and notes, memory writes, attach and patches are refused unless the server starts with `--allow-changes`; attach is always for your own processes.
