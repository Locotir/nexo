//! What the debugger's hooks, library-call trace and coverage need from a file: the places where the
//! program calls an import, the start of every basic block of a function, and C prototypes (the
//! person's, DWARF's, or the C library's) to read arguments with their types.

use std::collections::BTreeSet;

use iced_x86::{Decoder, DecoderOptions, FlowControl, OpKind};

pub use crate::decomp::types::{parse_proto, Proto};
use crate::{Format, Image};

/// A place where the program hands control to an imported function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSite {
    /// File address of the stub (`puts@plt`) or of the `call [slot]` instruction.
    pub addr: u64,
    /// The import's name.
    pub name: String,
    /// `None`: a stub, entered by a call (the return address is on the stack). `Some(len)`: the call
    /// instruction itself, `len` bytes long (the return address is the next instruction).
    pub call_len: Option<u8>,
}

/// Every stub an import goes through, and every `call [slot]` of an import in the program's code
/// (programs built without stubs, and every PE file). Sorted by address.
pub fn import_sites(img: &Image) -> Vec<ImportSite> {
    // the debugger runs 64-bit programs only
    if img.arm64 || img.bits == 32 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for f in img.funcs() {
        if let Some(n) = f.name.strip_suffix("@plt").filter(|_| img.format == Format::Elf) {
            out.push(ImportSite { addr: f.start, name: n.to_string(), call_len: None });
        }
    }
    let stubs: BTreeSet<u64> = out.iter().map(|s| s.addr).collect();
    for s in img.code_sections().filter(|s| !s.name.starts_with(".plt")) {
        let bytes = img.section_bytes(s);
        let mut d = Decoder::with_ip(64, bytes, s.addr, DecoderOptions::NONE);
        while d.can_decode() {
            let ins = d.decode();
            if ins.is_invalid() || ins.flow_control() != FlowControl::IndirectCall || !ins.is_ip_rel_memory_operand() {
                continue;
            }
            if stubs.contains(&ins.ip()) {
                continue;
            }
            if let Some(&i) = img.slots.get(&ins.ip_rel_memory_address()) {
                out.push(ImportSite {
                    addr: ins.ip(),
                    name: img.imports[i].name.clone(),
                    call_len: Some(ins.len() as u8),
                });
            }
        }
    }
    out.sort_by_key(|s| s.addr);
    out
}

/// The file addresses where the basic blocks of the code in `start..end` begin: the start, every
/// branch target inside the range, and the instruction after every branch, return or call that does
/// not come back. Read from `start` to `end` in one pass; only addresses where an instruction of that
/// pass begins are given, so a breakpoint on one never lands inside an instruction of that reading.
pub fn block_starts(img: &Image, start: u64, end: u64) -> Vec<u64> {
    // the debugger runs 64-bit programs only
    if img.arm64 || img.bits == 32 {
        return Vec::new();
    }
    let Some(bytes) = img.read_at(start, end.saturating_sub(start) as usize) else { return Vec::new() };
    let mut d = Decoder::with_ip(64, bytes, start, DecoderOptions::NONE);
    let mut starts = BTreeSet::new();
    let mut lead = BTreeSet::from([start]);
    let mut after_branch = false;
    while d.can_decode() {
        let ins = d.decode();
        if ins.is_invalid() {
            // data or a reading that went wrong: nothing past this point is trusted
            break;
        }
        starts.insert(ins.ip());
        if after_branch {
            lead.insert(ins.ip());
        }
        after_branch = false;
        match ins.flow_control() {
            FlowControl::ConditionalBranch | FlowControl::UnconditionalBranch => {
                if ins.op0_kind() == OpKind::NearBranch64 {
                    lead.insert(ins.near_branch_target());
                }
                after_branch = true;
            }
            FlowControl::IndirectBranch | FlowControl::Return | FlowControl::Exception | FlowControl::Interrupt => {
                after_branch = true
            }
            _ => {}
        }
    }
    lead.retain(|a| starts.contains(a));
    lead.into_iter().collect()
}

/// The prototype the person gave for the function at `addr`, or DWARF's.
pub fn proto_at(img: &Image, addr: u64) -> Option<Proto> {
    crate::decomp::types::protos(&img.user, img).remove(&addr)
}

/// How many arguments the decompiler's table says a library function takes (no types), and the
/// position of its format string when it is variadic.
pub fn arg_count(name: &str) -> Option<(usize, Option<usize>)> {
    crate::decomp::api::table(name).map(|s| (s.args, s.fmt))
}

/// C library functions with their types, for reading arguments in a library-call trace.
const LIBC: &[&str] = &[
    "size_t strlen(const char *s)",
    "size_t strnlen(const char *s, size_t maxlen)",
    "int strcmp(const char *s1, const char *s2)",
    "int strncmp(const char *s1, const char *s2, size_t n)",
    "int strcasecmp(const char *s1, const char *s2)",
    "int strncasecmp(const char *s1, const char *s2, size_t n)",
    "char *strcpy(char *dest, const char *src)",
    "char *strncpy(char *dest, const char *src, size_t n)",
    "char *strcat(char *dest, const char *src)",
    "char *strncat(char *dest, const char *src, size_t n)",
    "char *strchr(const char *s, int c)",
    "char *strrchr(const char *s, int c)",
    "char *strstr(const char *haystack, const char *needle)",
    "char *strdup(const char *s)",
    "char *strndup(const char *s, size_t n)",
    "char *strtok(char *str, const char *delim)",
    "char *strerror(int errnum)",
    "long strtol(const char *nptr, char **endptr, int base)",
    "unsigned long strtoul(const char *nptr, char **endptr, int base)",
    "long long strtoll(const char *nptr, char **endptr, int base)",
    "unsigned long long strtoull(const char *nptr, char **endptr, int base)",
    "int atoi(const char *nptr)",
    "long atol(const char *nptr)",
    "void *memcpy(void *dest, const void *src, size_t n)",
    "void *memmove(void *dest, const void *src, size_t n)",
    "void *memset(void *s, int c, size_t n)",
    "int memcmp(const void *s1, const void *s2, size_t n)",
    "void *memchr(const void *s, int c, size_t n)",
    "void *malloc(size_t size)",
    "void *calloc(size_t nmemb, size_t size)",
    "void *realloc(void *ptr, size_t size)",
    "void *reallocarray(void *ptr, size_t nmemb, size_t size)",
    "void free(void *ptr)",
    "int puts(const char *s)",
    "int putchar(int c)",
    "int printf(const char *format, ...)",
    "int fprintf(FILE *stream, const char *format, ...)",
    "int dprintf(int fd, const char *format, ...)",
    "int sprintf(char *str, const char *format, ...)",
    "int snprintf(char *str, size_t size, const char *format, ...)",
    "int __printf_chk(int flag, const char *format, ...)",
    "int __fprintf_chk(FILE *stream, int flag, const char *format, ...)",
    "int __sprintf_chk(char *str, int flag, size_t len, const char *format, ...)",
    "int __snprintf_chk(char *str, size_t maxlen, int flag, size_t len, const char *format, ...)",
    "int scanf(const char *format, ...)",
    "int sscanf(const char *str, const char *format, ...)",
    "int __isoc99_scanf(const char *format, ...)",
    "int __isoc99_sscanf(const char *str, const char *format, ...)",
    "int __isoc23_scanf(const char *format, ...)",
    "int __isoc23_sscanf(const char *str, const char *format, ...)",
    "int fputs(const char *s, FILE *stream)",
    "int fputc(int c, FILE *stream)",
    "int fgetc(FILE *stream)",
    "int getchar(void)",
    "char *fgets(char *s, int size, FILE *stream)",
    "size_t fread(void *ptr, size_t size, size_t nmemb, FILE *stream)",
    "size_t fwrite(const void *ptr, size_t size, size_t nmemb, FILE *stream)",
    "FILE *fopen(const char *pathname, const char *mode)",
    "FILE *fdopen(int fd, const char *mode)",
    "int fclose(FILE *stream)",
    "int fflush(FILE *stream)",
    "int fseek(FILE *stream, long offset, int whence)",
    "long ftell(FILE *stream)",
    "int open(const char *pathname, int flags, ...)",
    "int open64(const char *pathname, int flags, ...)",
    "int openat(int dirfd, const char *pathname, int flags, ...)",
    "int close(int fd)",
    "ssize_t read(int fd, void *buf, size_t count)",
    "ssize_t write(int fd, const void *buf, size_t count)",
    "int unlink(const char *pathname)",
    "int remove(const char *pathname)",
    "int rename(const char *oldpath, const char *newpath)",
    "int mkdir(const char *pathname, unsigned int mode)",
    "int rmdir(const char *pathname)",
    "int chdir(const char *path)",
    "int access(const char *pathname, int mode)",
    "char *getenv(const char *name)",
    "int setenv(const char *name, const char *value, int overwrite)",
    "int system(const char *command)",
    "void exit(int status)",
    "void _exit(int status)",
    "void abort(void)",
    "int abs(int j)",
    "int rand(void)",
    "void srand(unsigned int seed)",
    "long time(long *tloc)",
    "unsigned int sleep(unsigned int seconds)",
    "int usleep(unsigned int usec)",
    "int toupper(int c)",
    "int tolower(int c)",
    "int isatty(int fd)",
    "int socket(int domain, int type, int protocol)",
    "int connect(int sockfd, const void *addr, unsigned int addrlen)",
    "int bind(int sockfd, const void *addr, unsigned int addrlen)",
    "int listen(int sockfd, int backlog)",
    "int accept(int sockfd, void *addr, unsigned int *addrlen)",
    "ssize_t send(int sockfd, const void *buf, size_t len, int flags)",
    "ssize_t recv(int sockfd, void *buf, size_t len, int flags)",
    "int pthread_create(void *thread, const void *attr, void *start_routine, void *arg)",
    "int pthread_join(unsigned long thread, void **retval)",
    "int pthread_mutex_lock(void *mutex)",
    "int pthread_mutex_unlock(void *mutex)",
    "void *dlopen(const char *filename, int flags)",
    "void *dlsym(void *handle, const char *symbol)",
    "int __libc_start_main(void *main, int argc, char **argv, void *init, void *fini, void *rtld_fini, void *stack_end)",
    "void __stack_chk_fail(void)",
    "void __cxa_finalize(void *d)",
    "int *__errno_location(void)",
];

/// The C library's prototype of `name`, with its types.
pub fn libc_proto(name: &str) -> Option<Proto> {
    LIBC.iter().filter_map(|t| parse_proto(t).ok()).find(|p| p.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_c_library_prototype_in_the_table_parses() {
        for t in LIBC {
            assert!(parse_proto(t).is_ok(), "{t}");
        }
        let p = libc_proto("strlen").unwrap();
        assert_eq!(p.ret, "size_t");
        assert_eq!(p.args, vec![("const char *".to_string(), "s".to_string())]);
        assert!(libc_proto("printf").unwrap().variadic);
        assert!(libc_proto("no_such_function").is_none());
        assert_eq!(arg_count("puts"), Some((1, None)));
    }
}
