//! The glue between an opened file and the decompiler: names, strings, signatures, the cache and the op.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::flow::{Sig, Sigs};
use super::ir::Target;
use super::lift::{Abi, Env};
use super::render::{Line, Names};
use crate::image::Format;
use crate::page::{Output, Row};
use crate::{Error, Image, Result};

const VERSION: u32 = 1;

pub struct ImgEnv<'a>(pub &'a Image);

impl Env for ImgEnv<'_> {
    fn read(&self, addr: u64, len: usize) -> Option<&[u8]> {
        self.0.read_at(addr, len)
    }
    fn name_of(&self, addr: u64) -> Option<String> {
        plain_name(self.0, addr)
    }
    fn import_slot(&self, slot: u64) -> Option<usize> {
        self.0.slots.get(&slot).copied()
    }
    fn import_name(&self, i: usize) -> String {
        self.0.imports.get(i).map(|x| x.name.clone()).unwrap_or_default()
    }
    fn image_base(&self) -> u64 {
        self.0.image_base
    }
    fn is_pe(&self) -> bool {
        self.0.format == Format::Pe
    }
}

fn strip_plt(n: &str) -> String {
    n.strip_suffix("@plt").unwrap_or(n).to_string()
}

/// The name of whatever starts at `addr`: the person's name, a symbol, an import.
pub fn plain_name(img: &Image, addr: u64) -> Option<String> {
    if let Some(n) = img.user_name(addr) {
        return Some(n.to_string());
    }
    if let Some(n) = img.name_at(addr) {
        return Some(strip_plt(n));
    }
    let f = img.func_at(addr)?;
    (f.start == addr).then(|| strip_plt(img.func_name(f)))
}

pub struct ImgNames<'a> {
    pub img: &'a Image,
    pub vars: HashMap<String, String>,
    pub arg_types: HashMap<String, String>,
    pub structs: HashMap<String, Vec<super::types::Field>>,
}

pub fn escape(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars().take(80) {
        match c {
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            '\r' => o.push_str("\\r"),
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if c.is_control() => o.push_str(&format!("\\x{:02x}", c as u32)),
            c => o.push(c),
        }
    }
    if s.chars().count() > 80 {
        o.push_str("...");
    }
    o
}

impl Names for ImgNames<'_> {
    fn func(&self, addr: u64) -> Option<String> {
        plain_name(self.img, addr)
    }
    fn import(&self, i: usize) -> String {
        self.img.imports.get(i).map(|x| x.name.clone()).unwrap_or_else(|| format!("import_{i}"))
    }
    fn string(&self, addr: u64) -> Option<String> {
        let (s, off) = self.img.string_at(addr)?;
        (off == 0 && !s.wide).then(|| escape(&s.text))
    }
    fn global(&self, addr: u64) -> Option<String> {
        let s = self.img.section_at(addr)?;
        if let Some(n) = plain_name(self.img, addr) {
            return Some(n);
        }
        if let Some((n, off)) = self.img.symbol_covering(addr) {
            return Some(if off == 0 { n.to_string() } else { format!("{n}+{off:#x}") });
        }
        if s.exec {
            return None;
        }
        Some(format!("g_{addr:x}"))
    }
    fn var_name(&self, key: &str) -> Option<String> {
        self.vars.get(key).cloned()
    }
    fn field(&self, key: &str, off: u64) -> Option<String> {
        let sname = super::types::struct_of(self.arg_types.get(key)?)?;
        self.structs.get(&sname)?.iter().find(|f| f.off == off).map(|f| f.name.clone())
    }
}

// ---------------------------------------------------------------------------------------------
// Signatures

pub(crate) fn table(name: &str) -> Option<Sig> {
    let s = |args: usize| Sig { args, ..Sig::default() };
    let v = |args: usize| Sig { args, void: true, ..Sig::default() };
    let f = |args: usize, fmt: usize| Sig { args, fmt: Some(fmt), ..Sig::default() };
    Some(match name {
        "printf" | "scanf" | "g_print" | "g_printerr" | "g_strdup_printf" | "wprintf" => f(1, 0),
        "fprintf" | "sprintf" | "sscanf" | "fscanf" | "dprintf" | "g_string_printf" | "g_string_append_printf" => {
            f(2, 1)
        }
        "snprintf" | "g_snprintf" => f(3, 2),
        "__printf_chk" => f(2, 1),
        "__fprintf_chk" | "__sprintf_chk" => f(3, 2),
        "__snprintf_chk" => f(5, 4),
        "puts" | "strlen" | "atoi" | "atol" | "atof" | "getenv" | "strdup" | "malloc" | "free_" | "fclose"
        | "close" | "toupper" | "tolower" | "isdigit" | "isalpha" | "isspace" | "isalnum" | "isupper" | "islower"
        | "putchar" | "srand" | "usleep" | "sleep" | "fflush" | "rewind" | "chdir" | "rmdir" | "unlink"
        | "g_strdup" | "g_malloc" | "g_malloc0" | "fgetc" | "getc" | "fileno" | "ftell" | "opendir" | "closedir"
        | "readdir" | "strerror" | "perror" | "system" | "abs" | "labs" | "ferror" | "feof" | "strtod_"
        | "setlocale_" | "rand_r" | "gmtime" | "localtime" | "sqrt" => s(1),
        "free" | "exit" | "_exit" | "srandom" | "g_free" | "g_object_unref" => v(1),
        "abort" | "__stack_chk_fail" => v(0),
        "__ctype_toupper_loc"
        | "__ctype_tolower_loc"
        | "__ctype_b_loc"
        | "__errno_location"
        | "g_ptr_array_new"
        | "doupdate"
        | "rand"
        | "getchar"
        | "random"
        | "getpid"
        | "fork"
        | "getuid"
        | "clock" => s(0),
        "time" | "ctime" | "fputc_" | "putc_" => s(1),
        "strcmp"
        | "strcpy"
        | "strcat"
        | "strchr"
        | "strrchr"
        | "strstr"
        | "strcasecmp"
        | "fputs"
        | "fputc"
        | "putc"
        | "fopen"
        | "access"
        | "mkdir"
        | "rename"
        | "realloc"
        | "calloc"
        | "strpbrk"
        | "strcspn"
        | "strspn"
        | "strtok"
        | "setlocale"
        | "getcwd"
        | "fgetpos"
        | "g_strconcat_"
        | "pow"
        | "fmod"
        | "atan2"
        | "ungetc"
        | "strcoll"
        | "gettimeofday"
        | "nanosleep"
        | "g_hash_table_lookup" => s(2),
        "strncmp"
        | "strncpy"
        | "strncat"
        | "memcpy"
        | "memmove"
        | "memset"
        | "memcmp"
        | "strncasecmp"
        | "fgets"
        | "read"
        | "write"
        | "open"
        | "strtol"
        | "strtoul"
        | "strtod"
        | "lseek"
        | "memchr"
        | "fseek"
        | "mmap_"
        | "signal_"
        | "__strcpy_chk"
        | "g_strndup"
        | "send_"
        | "bsearch_"
        | "g_hash_table_insert" => s(3),
        "fread" | "fwrite" | "qsort" | "__memcpy_chk" | "__memset_chk" | "__memmove_chk" | "__strncpy_chk" => s(4),
        "__fgets_chk" => s(4),
        "__isoc99_sscanf" | "__isoc99_fscanf" => f(2, 1),
        "__isoc99_scanf" => f(1, 0),
        "wgetch"
        | "wrefresh"
        | "wclear"
        | "werase"
        | "wclrtoeol"
        | "wclrtobot"
        | "wnoutrefresh"
        | "doupdate_"
        | "wstandout"
        | "wstandend"
        | "keypad_"
        | "g_string_new"
        | "g_utf8_get_char"
        | "g_strfreev_"
        | "g_ascii_strup_"
        | "g_hash_table_size"
        | "g_ptr_array_new_"
        | "g_list_length"
        | "g_list_first"
        | "g_list_last"
        | "g_strdup_"
        | "getpwuid"
        | "gethostbyname"
        | "inet_ntoa"
        | "mkstemp"
        | "tmpfile_"
        | "isatty"
        | "g_hash_table_destroy_"
        | "g_string_free_" => s(1),
        "g_strfreev"
        | "g_ptr_array_free_"
        | "g_hash_table_destroy"
        | "g_list_free"
        | "g_ptr_array_unref"
        | "endwin_" => v(1),
        "wattrset"
        | "wattron"
        | "wattroff"
        | "waddch"
        | "g_string_append"
        | "g_string_free"
        | "g_string_append_c"
        | "g_utf8_strlen"
        | "g_string_prepend"
        | "g_hash_table_new"
        | "listen"
        | "g_hash_table_remove"
        | "g_list_nth"
        | "g_list_append_"
        | "g_ascii_strcasecmp"
        | "g_str_has_prefix"
        | "g_str_has_suffix"
        | "g_ptr_array_index_"
        | "g_string_truncate"
        | "g_string_set_size"
        | "g_utf8_offset_to_pointer"
        | "g_unichar_to_utf8"
        | "wbkgd"
        | "mvwin_"
        | "g_ascii_strncasecmp_" => s(2),
        "g_string_insert_len"
        | "g_string_insert"
        | "wmove"
        | "waddnstr"
        | "waddstr_"
        | "socket"
        | "connect"
        | "bind"
        | "accept"
        | "getsockname"
        | "g_strsplit"
        | "g_utf8_find_next_char_"
        | "g_string_append_len"
        | "g_strndup_"
        | "g_hash_table_replace"
        | "g_hash_table_insert_"
        | "wresize"
        | "whline"
        | "wvline"
        | "g_ptr_array_add_"
        | "__isoc23_strtol"
        | "__isoc23_strtoul"
        | "__isoc23_strtoll"
        | "__isoc23_strtoull"
        | "g_utf8_strncpy"
        | "g_string_erase" => s(3),
        "recv"
        | "send"
        | "g_string_insert_c_"
        | "mvwaddstr_"
        | "g_strsplit_set_"
        | "g_hash_table_lookup_extended"
        | "waddchnstr_"
        | "g_hash_table_foreach_" => s(4),
        "select" | "setsockopt" | "getsockopt" | "recvfrom_" | "mvwaddnstr" => s(5),
        "g_log" => f(3, 2),
        "__isoc23_sscanf" | "__isoc23_fscanf" => f(2, 1),
        "__isoc23_scanf" => f(1, 0),
        "mvwprintw" => f(4, 3),
        "wprintw" => f(2, 1),
        "ExitProcess" | "Sleep" | "CloseHandle" | "GetProcAddress_" | "LoadLibraryA" | "LoadLibraryW" => s(1),
        "GetProcAddress" | "GetModuleHandleExW_" => s(2),
        "ReadFile" | "WriteFile" => s(5),
        "CreateFileW" | "CreateFileA" => s(7),
        "MessageBoxW" | "MessageBoxA" => s(4),
        _ => return None,
    })
}

pub struct ImgSigs<'a> {
    pub img: &'a Image,
    pub protos: HashMap<u64, super::types::Proto>,
    pub kept: HashMap<u64, Vec<u8>>,
}

impl Sigs for ImgSigs<'_> {
    fn sig_of(&mut self, t: &Target) -> Option<Sig> {
        let (addr, name) = match t {
            Target::Import(i) => (None, self.img.imports.get(*i)?.name.clone()),
            Target::Direct(a) => (Some(*a), plain_name(self.img, *a).unwrap_or_default()),
            Target::Indirect(_) => return None,
        };
        if let Some(a) = addr {
            if let Some(p) = self.protos.get(&a) {
                return Some(Sig { args: p.args.len(), fmt: None, void: p.ret == "void", float: Vec::new() });
            }
        }
        table(&name)
    }
    fn keeps(&mut self, t: &Target) -> Vec<u8> {
        let Target::Direct(a) = t else { return Vec::new() };
        if let Some(k) = self.kept.get(a) {
            return k.clone();
        }
        let k = match crate::ops::resolve(self.img, &format!("{a:#x}")) {
            Ok(fx) if fx.start == *a => {
                super::flow::unwritten_regs(&ImgEnv(self.img), fx.start, fx.end, abi_of(self.img))
            }
            _ => Vec::new(),
        };
        self.kept.insert(*a, k.clone());
        k
    }
    fn cstr(&self, addr: u64) -> Option<String> {
        let (s, off) = self.img.string_at(addr)?;
        (off == 0 && !s.wide).then(|| s.text.clone())
    }
}

// ---------------------------------------------------------------------------------------------
// Cache and op

/// Names file plus the decompiler sources: a new build never reads an old answer.
fn names_digest(img: &Image, data_dir: &Path) -> String {
    let p = crate::names::file_path(data_dir, &img.sha256);
    let mut h = Sha256::new();
    h.update(std::fs::read(&p).unwrap_or_default());
    for src in [
        include_str!("ir.rs"),
        include_str!("lift.rs"),
        include_str!("flow.rs"),
        include_str!("divrec.rs"),
        include_str!("structure.rs"),
        include_str!("render.rs"),
        include_str!("api.rs"),
        include_str!("../dwarf.rs"),
    ] {
        h.update(src.as_bytes());
    }
    h.finalize().iter().take(8).map(|x| format!("{x:02x}")).collect()
}

fn cache_path(img: &Image, data_dir: &Path, start: u64) -> std::path::PathBuf {
    data_dir.join("bin").join(format!("{}.decomp", img.sha256)).join(format!("{start:x}.json"))
}

fn read_cache(path: &Path, digest: &str) -> Option<Vec<Line>> {
    let v: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    if v["version"].as_u64()? != u64::from(VERSION) || v["names"].as_str()? != digest {
        return None;
    }
    v["lines"]
        .as_array()?
        .iter()
        .map(|l| Some(Line { addr: l[0].as_u64(), indent: l[1].as_u64()? as usize, text: l[2].as_str()?.to_string() }))
        .collect()
}

fn write_cache(path: &Path, digest: &str, lines: &[Line]) {
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let v = json!({
        "version": VERSION,
        "names": digest,
        "lines": lines.iter().map(|l| json!([l.addr, l.indent, l.text])).collect::<Vec<_>>(),
    });
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    if std::fs::write(&tmp, v.to_string()).is_ok() && std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

pub fn abi_of(img: &Image) -> Abi {
    if img.format == Format::Pe {
        Abi::Win64
    } else {
        Abi::SysV
    }
}

/// Decompile one function (from the cache when it is there).
pub fn one(img: &Image, data_dir: &Path, target: &str) -> Result<(String, Vec<Line>, bool)> {
    let f = crate::ops::resolve(img, target)?;
    img.require_x86(&format!("decomp of {} ({:#x})", img.func_name(f), f.start))?;
    let digest = names_digest(img, data_dir);
    let cp = cache_path(img, data_dir, f.start);
    let name = img.func_name(f).to_string();
    if std::env::var_os("NEXO_DECOMP_IR").is_none() {
        if let Some(lines) = read_cache(&cp, &digest) {
            return Ok((name, lines, true));
        }
    }
    let user = &img.user;
    let protos = super::types::protos(user, img);
    let proto = protos.get(&f.start).cloned();
    let mut vars: HashMap<String, String> = HashMap::new();
    let mut arg_types: HashMap<String, String> = HashMap::new();
    if let Some(p) = &proto {
        for (i, (t, n)) in p.args.iter().enumerate() {
            vars.insert(format!("a{}", i + 1), n.clone());
            arg_types.insert(format!("a{}", i + 1), t.clone());
        }
    }
    if let Some(m) = img.dwarf().slot_types.get(&f.start) {
        arg_types.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    if let Some(m) = img.dwarf().vars.get(&f.start) {
        vars.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    if let Some(m) = user.vars.get(&f.start) {
        vars.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    let names = ImgNames { img, vars, arg_types, structs: super::types::structs(user, img) };
    let mut sigs = ImgSigs { img, protos, kept: HashMap::new() };
    let mut lines =
        super::decompile_with(&ImgEnv(img), f.start, f.end, abi_of(img), &mut sigs, &names, &name, proto.as_ref());
    let nl = lines.iter().filter(|l| l.text.contains("/* not lifted")).count();
    let argc = lines.first().map_or(0, |l| match l.text.split_once('(') {
        Some((_, r)) if !r.starts_with("void)") => r.matches(", ").count() + 1,
        _ => 0,
    });
    let head = format!("// {name} {:#x} {} bytes, {argc} args, {nl} not lifted", f.start, f.end - f.start);
    lines.insert(0, Line { addr: None, indent: 0, text: head });
    write_cache(&cp, &digest, &lines);
    Ok((name, lines, false))
}

pub fn line_text(l: &Line, with_addr: bool) -> String {
    let pad = "    ".repeat(l.indent);
    if with_addr {
        match l.addr {
            Some(a) => format!("{a:#010x}  {pad}{}", l.text),
            None => format!("{:10}  {pad}{}", "", l.text),
        }
    } else {
        format!("{pad}{}", l.text)
    }
}

/// The pseudo-C of one or more functions, paged by line.
pub fn decomp_window(
    img: &Image,
    data_dir: &Path,
    targets: &[String],
    with_addr: bool,
    from: Option<u64>,
    budget: usize,
) -> Result<Output> {
    if targets.is_empty() {
        return Err(Error::Usage("give an address (0x401000) or a function name".into()));
    }
    let _ = img.refs();
    let mut rows: Vec<Row> = Vec::new();
    let mut cached_all = true;
    let mut meta_fns: Vec<Value> = Vec::new();
    for (i, t) in targets.iter().enumerate() {
        let (name, lines, cached) = one(img, data_dir, t)?;
        cached_all &= cached;
        meta_fns.push(json!({"name": name, "lines": lines.len(), "cached": cached}));
        if i > 0 {
            let key = rows.len() as u64;
            rows.push(Row { key, text: String::new(), json: None });
        }
        for l in lines {
            let key = rows.len() as u64;
            rows.push(Row {
                key,
                text: line_text(&l, with_addr),
                json: Some(json!({"addr": l.addr, "indent": l.indent, "text": l.text})),
            });
        }
    }
    let _ = (from, budget);
    let joined = targets.iter().map(|t| crate::page::shell_word(t)).collect::<Vec<_>>().join(" ");
    let flag = if with_addr { " --addr" } else { "" };
    Ok(Output {
        op: "decomp",
        meta: json!({"functions": meta_fns, "cached": cached_all}),
        rows,
        tail: 0,
        command: format!("{}{flag}", crate::ops::command(img, "decomp", &joined)),
    })
}

fn line_out(img: &Image, op: &'static str, text: String, j: Value) -> Output {
    Output {
        op,
        meta: j.clone(),
        rows: vec![Row { key: 0, text, json: Some(j) }],
        tail: 0,
        command: crate::ops::command(img, op, ""),
    }
}

/// Give a function a C prototype (`--clear` removes it).
pub fn proto_set(img: &Image, data_dir: &Path, target: &str, text: Option<&str>) -> Result<Output> {
    let f = crate::ops::resolve(img, target)?;
    let start = f.start;
    match text {
        Some(t) => {
            let p = super::types::parse_proto(t).map_err(Error::Bad)?;
            let shown = format!(
                "{} {}({})",
                p.ret,
                p.name,
                p.args.iter().map(|a| format!("{} {}", a.0, a.1)).collect::<Vec<_>>().join(", ")
            );
            crate::names::edit(data_dir, &img.sha256, |n| {
                n.protos.insert(start, t.trim().trim_end_matches(';').to_string());
                Ok(())
            })?;
            Ok(line_out(
                img,
                "proto",
                format!("prototype of {start:#x}: {shown}"),
                json!({"addr": start, "proto": shown}),
            ))
        }
        None => {
            let had = crate::names::edit(data_dir, &img.sha256, |n| Ok(n.protos.remove(&start).is_some()))?;
            Ok(line_out(
                img,
                "proto",
                format!("cleared the prototype of {start:#x} (had one: {had})"),
                json!({"addr": start, "cleared": had}),
            ))
        }
    }
}

/// Define a small struct: fields as `offset:type:name`.
pub fn struct_set(img: &Image, data_dir: &Path, name: &str, fields: Option<&str>) -> Result<Output> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(Error::Bad(format!("{name:?} is not a struct name (letters, digits, _)")));
    }
    match fields {
        Some(t) => {
            let f = super::types::parse_struct(t).map_err(Error::Bad)?;
            crate::names::edit(data_dir, &img.sha256, |n| {
                n.structs.insert(name.to_string(), t.trim().to_string());
                Ok(())
            })?;
            Ok(line_out(
                img,
                "struct",
                format!("struct {name}: {} fields", f.len()),
                json!({"name": name, "fields": f.len()}),
            ))
        }
        None => {
            let had = crate::names::edit(data_dir, &img.sha256, |n| Ok(n.structs.remove(name).is_some()))?;
            Ok(line_out(
                img,
                "struct",
                format!("cleared struct {name} (had one: {had})"),
                json!({"name": name, "cleared": had}),
            ))
        }
    }
}

/// Rename a variable of one function: `a1`, `v3` or `local_18` becomes `name` (`--clear` undoes it).
pub fn var_set(img: &Image, data_dir: &Path, target: &str, key: &str, name: Option<&str>) -> Result<Output> {
    let f = crate::ops::resolve(img, target)?;
    let start = f.start;
    let key_ok = key.len() > 1
        && (key.starts_with('a')
            || key.starts_with('v')
            || key.starts_with('f')
            || key.starts_with("local_")
            || key.starts_with("stack_"))
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !key_ok {
        return Err(Error::Bad(format!("{key:?} is not a variable of the output (a1, v3, local_18, stack_0)")));
    }
    match name {
        Some(n) => {
            if n.is_empty()
                || !n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                || n.chars().next().is_some_and(|c| c.is_ascii_digit())
            {
                return Err(Error::Bad(format!("{n:?} is not a C name (letters, digits, _)")));
            }
            crate::names::edit(data_dir, &img.sha256, |u| {
                u.vars.entry(start).or_default().insert(key.to_string(), n.to_string());
                Ok(())
            })?;
            Ok(line_out(
                img,
                "var",
                format!("{key} is now {n} in {start:#x}"),
                json!({"addr": start, "key": key, "name": n}),
            ))
        }
        None => {
            let had = crate::names::edit(data_dir, &img.sha256, |u| {
                let r = u.vars.get_mut(&start).is_some_and(|m| m.remove(key).is_some());
                if u.vars.get(&start).is_some_and(|m| m.is_empty()) {
                    u.vars.remove(&start);
                }
                Ok(r)
            })?;
            Ok(line_out(
                img,
                "var",
                format!("cleared the name of {key} in {start:#x} (had one: {had})"),
                json!({"addr": start, "key": key, "cleared": had}),
            ))
        }
    }
}
