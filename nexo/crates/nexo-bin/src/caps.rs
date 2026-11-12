//! What an imported function is for, by name. A name is a hint about capability, not proof of use:
//! a program can also load functions at run time (see the dynamic loading group).

/// (group, exact names, name prefixes). The first group that matches wins, in this order.
const GROUPS: [(&str, &[&str], &[&str]); 7] = [
    (
        "network",
        &[
            "socket",
            "connect",
            "bind",
            "listen",
            "accept",
            "accept4",
            "send",
            "sendto",
            "sendmsg",
            "recv",
            "recvfrom",
            "recvmsg",
            "getaddrinfo",
            "getnameinfo",
            "gethostbyname",
            "gethostbyname2",
            "gethostbyaddr",
            "getsockopt",
            "setsockopt",
            "getpeername",
            "getsockname",
            "inet_addr",
            "inet_aton",
            "inet_ntoa",
            "inet_pton",
            "inet_ntop",
            "closesocket",
            "ioctlsocket",
            "freeaddrinfo",
            "if_nametoindex",
        ],
        &[
            "WSA",
            "Internet",
            "Http",
            "WinHttp",
            "URLDownload",
            "URLOpen",
            "curl_",
            "SSL_",
            "gnutls_",
            "Dns",
            "mbedtls_ssl",
        ],
    ),
    (
        "processes",
        &[
            "fork",
            "vfork",
            "execve",
            "execv",
            "execvp",
            "execl",
            "execlp",
            "execle",
            "execvpe",
            "system",
            "popen",
            "posix_spawn",
            "posix_spawnp",
            "clone",
            "kill",
            "ptrace",
            "prctl",
            "wait",
            "waitpid",
            "wait4",
            "setuid",
            "setgid",
            "seteuid",
            "setreuid",
            "daemon",
            "pthread_create",
            "WinExec",
            "OpenProcess",
            "TerminateProcess",
            "VirtualAllocEx",
            "WriteProcessMemory",
            "ReadProcessMemory",
            "CreateRemoteThread",
            "CreateThread",
            "OpenThread",
            "SuspendThread",
            "ResumeThread",
            "QueueUserAPC",
            "CreateToolhelp32Snapshot",
            "IsDebuggerPresent",
            "CheckRemoteDebuggerPresent",
            "NtQueryInformationProcess",
            "SetWindowsHookExA",
            "SetWindowsHookExW",
        ],
        &["CreateProcess", "ShellExecute", "Process32", "_beginthread", "_exec", "_spawn", "_wsystem", "NtCreate"],
    ),
    (
        "crypto",
        &[
            "getrandom",
            "arc4random",
            "arc4random_buf",
            "getentropy",
            "RtlGenRandom",
            "SystemFunction036",
            "CryptUnprotectData",
            "CryptProtectData",
            "MD5",
            "SHA1",
            "SHA256",
            "SHA512",
            "RAND_bytes",
        ],
        &[
            "EVP_", "SHA1_", "SHA256_", "SHA384_", "SHA512_", "SHA224_", "MD5_", "AES_", "DES_", "RSA_", "RAND_",
            "HMAC", "BN_", "EC_", "ECDSA", "ECDH", "X509", "PEM_", "CRYPTO_", "OPENSSL_", "gcry_", "mbedtls_",
            "sodium_", "crypto_", "Crypt", "BCrypt", "NCrypt", "Cert", "nettle_", "wc_",
        ],
    ),
    (
        "registry",
        &["SHGetValueA", "SHGetValueW", "SHSetValueA", "SHSetValueW", "SHDeleteValueA", "SHDeleteValueW"],
        &[
            "RegOpenKey",
            "RegCreateKey",
            "RegSetValue",
            "RegSetKeyValue",
            "RegQuery",
            "RegDelete",
            "RegEnum",
            "RegGetValue",
            "RegCloseKey",
            "RegFlushKey",
            "RegLoad",
            "RegSave",
            "RegNotify",
            "NtOpenKey",
            "NtSetValueKey",
            "NtQueryValueKey",
            "ZwOpenKey",
            "ZwSetValueKey",
            "ZwQueryValueKey",
            "ZwCreateKey",
        ],
    ),
    (
        "dynamic loading",
        &[
            "dlopen",
            "dlsym",
            "dlclose",
            "dlvsym",
            "dlmopen",
            "GetProcAddress",
            "FreeLibrary",
            "LdrLoadDll",
            "LdrGetProcedureAddress",
            "LdrGetDllHandle",
        ],
        &["LoadLibrary", "GetModuleHandle", "LoadPackagedLibrary"],
    ),
    (
        "files",
        &[
            "open",
            "open64",
            "openat",
            "creat",
            "read",
            "write",
            "pread",
            "pwrite",
            "pread64",
            "pwrite64",
            "fopen",
            "fopen64",
            "freopen",
            "fread",
            "fwrite",
            "fgets",
            "fputs",
            "fseek",
            "fseeko",
            "unlink",
            "unlinkat",
            "remove",
            "rename",
            "renameat",
            "mkdir",
            "rmdir",
            "stat",
            "stat64",
            "lstat",
            "fstat",
            "fstat64",
            "access",
            "chmod",
            "chown",
            "opendir",
            "readdir",
            "readdir64",
            "closedir",
            "realpath",
            "truncate",
            "ftruncate",
            "mkstemp",
            "tmpfile",
            "sendfile",
            "ReadFile",
            "WriteFile",
            "MapViewOfFile",
            "fopen_s",
            "_wfopen",
            "_open",
            "_wopen",
            "symlink",
            "readlink",
            "link",
            "scandir",
            "glob",
            "nftw",
            "fts_open",
        ],
        &[
            "CreateFile",
            "DeleteFile",
            "MoveFile",
            "CopyFile",
            "FindFirstFile",
            "FindNextFile",
            "GetFileAttributes",
            "SetFileAttributes",
            "CreateDirectory",
            "RemoveDirectory",
            "GetTempPath",
            "GetTempFileName",
            "GetFileSize",
            "SetFilePointer",
            "NtCreateFile",
            "NtReadFile",
            "NtWriteFile",
        ],
    ),
    (
        "memory",
        &["mmap", "mmap64", "mprotect", "munmap", "mremap", "memfd_create", "VirtualFree", "HeapCreate"],
        &["VirtualAlloc", "VirtualProtect", "NtAllocateVirtualMemory", "NtProtectVirtualMemory"],
    ),
];

/// The names of the groups, in the order they are listed.
pub fn group_names() -> impl Iterator<Item = &'static str> {
    GROUPS.iter().map(|g| g.0)
}

/// The group of an imported name (version suffixes like `@GLIBC_2.2.5` and leading underscores ignored).
pub fn group_of(name: &str) -> Option<&'static str> {
    let base = name.split('@').next().unwrap_or(name);
    let base = base.strip_prefix("__imp_").unwrap_or(base);
    let trimmed = base.trim_start_matches('_');
    let hit = |n: &str| {
        GROUPS.iter().find(|(_, exact, prefixes)| exact.contains(&n) || prefixes.iter().any(|p| n.starts_with(p)))
    };
    // `read` and `_read`, `__isoc99_*` are the same call; a lower-case name is looked up without the underscores
    hit(base).or_else(|| hit(trimmed)).map(|g| g.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_fall_in_the_groups_their_names_say() {
        assert_eq!(group_of("socket@GLIBC_2.2.5"), Some("network"));
        assert_eq!(group_of("InternetOpenUrlA"), Some("network"));
        assert_eq!(group_of("CreateProcessW"), Some("processes"));
        assert_eq!(group_of("EVP_EncryptInit_ex"), Some("crypto"));
        assert_eq!(group_of("RegSetValueExW"), Some("registry"));
        assert_eq!(group_of("GetProcAddress"), Some("dynamic loading"));
        assert_eq!(group_of("fopen"), Some("files"));
        assert_eq!(group_of("_open"), Some("files"));
        assert_eq!(group_of("VirtualProtect"), Some("memory"));
        assert_eq!(group_of("printf"), None);
        // a window class is not the registry
        assert_eq!(group_of("RegisterClassExW"), None);
    }
}
