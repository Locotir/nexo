//! Debug info that is damaged: the line table and the variables are read from mutated copies of a real
//! program's DWARF, and must come back as `None` or as something smaller, never a panic or a hang.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use nexo_bin::Image;

use crate::lines::Lines;
use crate::vars::Vars;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// A small C program with structs, arrays, pointers and several functions, built with `-g`.
fn program() -> Option<Vec<u8>> {
    let dir = tempfile::tempdir().ok()?;
    let c = dir.path().join("p.c");
    std::fs::write(
        &c,
        "#include <string.h>\nstruct node { int v; struct node *next; char name[16]; double w[3]; };\n\
         typedef struct node node_t;\nunion u { int i; float f; };\nenum color { RED, GREEN = 5, BLUE };\n\
         static int grid[4][5];\nint sum(node_t *n, int k) { int t = 0; for (int i = 0; n && i < k; i++) { t += n->v + (int)n->w[1]; n = n->next; } return t; }\n\
         int main(int argc, char **argv) { node_t a; union u q; enum color c = BLUE; memset(&a, 0, sizeof a); a.v = argc; q.i = 3; grid[1][2] = c;\n\
         strcpy(a.name, argv[0]); return sum(&a, 1) + q.i + grid[1][2]; }\n",
    )
    .ok()?;
    let exe = dir.path().join("p.out");
    let ok =
        std::process::Command::new("cc").args(["-g", "-O0", "-o"]).arg(&exe).arg(&c).output().ok()?.status.success();
    ok.then(|| std::fs::read(&exe).ok()).flatten()
}

fn use_image(bytes: Vec<u8>) {
    let Ok(img) = Image::from_bytes(bytes, PathBuf::from("/x")) else { return };
    if let Some(l) = Lines::of(&img) {
        for a in [0u64, 0x1000, 0x1139, 0x1140, u64::MAX] {
            let _ = l.at(a);
            let _ = l.after_prologue(a);
        }
        let _ = l.line_addr("p.c", 1);
        let _ = l.line_addr("p.c", u32::MAX);
    }
    let _ = Vars::of(&img);
}

#[test]
fn damaged_line_tables_and_variables_never_panic_or_hang() {
    let Some(good) = program() else {
        eprintln!("skipped: no C compiler");
        return;
    };
    let img = Image::from_bytes(good.clone(), PathBuf::from("/x")).unwrap();
    use_image(good.clone());
    let spans: Vec<(usize, usize)> = img
        .sections
        .iter()
        .filter(|s| s.name.starts_with(".debug_") && s.file_size > 0)
        .map(|s| (s.file_off as usize, (s.file_off + s.file_size) as usize))
        .filter(|&(a, b)| b <= good.len() && a < b)
        .collect();
    assert!(!spans.is_empty(), "the program has no DWARF");
    let cases: usize = std::env::var("NEXO_DBG_HARDEN_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(1200);
    let mut r =
        Rng(std::env::var("NEXO_DBG_HARDEN_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(0x1234_5678_9abc_def1));
    for i in 0..cases {
        let mut b = good.clone();
        for _ in 0..1 + r.next() % 4 {
            let (a, e) = spans[(r.next() as usize) % spans.len()];
            let at = a + (r.next() as usize) % (e - a);
            match r.next() % 4 {
                0 => b[at] ^= 1 << (r.next() % 8),
                1 => b[at] = [0, 0xff, 0x7f, 0x80][(r.next() % 4) as usize],
                2 => {
                    let n = 4.min(e - at);
                    let v = [u32::MAX, 0, 1 << 31, 0x00ff_ffff][(r.next() % 4) as usize].to_le_bytes();
                    b[at..at + n].copy_from_slice(&v[..n]);
                }
                _ => b[at] = r.next() as u8,
            }
        }
        let t0 = Instant::now();
        let h = std::thread::Builder::new().name("dbg-harden".into()).spawn(move || use_image(b)).unwrap();
        let end = Instant::now() + Duration::from_secs(20);
        while !h.is_finished() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(h.is_finished(), "case {i}: still running after 20 s");
        if let Err(p) = h.join() {
            let m = p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()));
            panic!("case {i} panicked: {m:?}");
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "case {i} took {:?}", t0.elapsed());
    }
}
