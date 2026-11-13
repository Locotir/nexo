//! Linux x86-64 system calls: names, error names, how the common ones take their arguments, and
//! the line a trace shows for one. The name and error tables come from the kernel headers
//! (`asm/unistd_64.h`, `asm-generic/errno*.h`).

mod classes;
mod decode;
mod tables;

pub use classes::Filter;
pub use decode::{args_at_entry, call_text, fill, ret_text};
pub use tables::name;

#[cfg(test)]
mod tests {
    use super::decode::{flags, MAP, OPEN};
    use super::tables::{errno_name, number, NAMES};
    use super::*;

    #[test]
    fn tables_and_decoding() {
        assert_eq!(name(0), Some("read"));
        assert_eq!(name(257), Some("openat"));
        assert_eq!(number("exit_group"), Some(231));
        assert_eq!(errno_name(2), Some("ENOENT"));
        assert_eq!(errno_name(133), Some("EHWPOISON"));
        assert!(NAMES.windows(2).all(|w| w[0].0 < w[1].0));
        let mut mem = |addr: u64, len: usize| -> Vec<u8> {
            let s: &[u8] = if addr == 0x1000 { b"/no/such\0" } else { b"hello world\n" };
            s[..s.len().min(len)].to_vec()
        };
        let a = [(-100i64) as u64, 0x1000, 0o2000000, 0, 0, 0];
        let args = args_at_entry("openat", &a, &mut mem).unwrap();
        assert_eq!(call_text("openat", Some(&args), &a), "openat(AT_FDCWD, \"/no/such\", O_RDONLY|O_CLOEXEC)");
        let a = [1, 0x2000, 12, 0, 0, 0];
        let args = args_at_entry("write", &a, &mut mem).unwrap();
        assert_eq!(args, vec!["1", "\"hello world\\n\"", "12"]);
        let a = [0, 0x2000, 4096, 0, 0, 0];
        let mut args = args_at_entry("read", &a, &mut mem).unwrap();
        assert_eq!(args[1], "0x2000");
        fill("read", &a, 5, false, &mut mem, &mut args);
        assert_eq!(args[1], "\"hello\"");
        assert_eq!(ret_text("openat", -2, true), "-1 ENOENT");
        assert_eq!(flags(0x22, MAP, ""), "MAP_PRIVATE|MAP_ANONYMOUS");
        assert_eq!(flags(0o1101, OPEN, "O_RDONLY"), "O_WRONLY|O_CREAT|O_TRUNC");
        let f = Filter::parse(&["file".into(), "mmap".into()]).unwrap();
        assert!(f.takes("openat") && f.takes("mmap") && !f.takes("socket"));
        assert!(Filter::parse(&["nonsense".into()]).is_err());
    }
}
