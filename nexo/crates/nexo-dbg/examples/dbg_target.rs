//! A small program for the debugger's tests: a known function, a loop, a global, and a few modes.
//!   (none) | loop   add 1..=5 through `dbg_add` into `DBG_COUNTER`, then exit
//!   wait            sleep until killed
//!   crash           read through a null pointer
//!   echo            copy lines from standard input to standard output until it closes
//!   thread          a second thread runs `dbg_add` in a loop
//!   attach          allow any process of this user to attach, then sleep until killed

use std::io::BufRead;
use std::sync::atomic::{AtomicU64, Ordering};

#[no_mangle]
pub static DBG_COUNTER: AtomicU64 = AtomicU64::new(0);

#[no_mangle]
#[inline(never)]
pub extern "C" fn dbg_add(a: u64, b: u64) -> u64 {
    std::hint::black_box(a + b)
}

#[no_mangle]
#[inline(never)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn dbg_crash(p: *const u64) -> u64 {
    // A plain load, not read_volatile: Rust before 1.90 checks read_volatile's pointer in debug builds and aborts in
    // libc, and the tests want the fault on this function's own load whatever the toolchain.
    let v: u64;
    #[cfg(target_arch = "x86_64")]
    // SAFETY: not safe on purpose, this is the crash the debugger tests look at.
    unsafe {
        std::arch::asm!("mov {}, qword ptr [{}]", out(reg) v, in(reg) p); // the faulting load
    }
    #[cfg(not(target_arch = "x86_64"))]
    // SAFETY: as above.
    unsafe {
        v = std::ptr::read_volatile(p); // the faulting load
    }
    v
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "loop".into());
    match mode.as_str() {
        "wait" => loop {
            std::thread::sleep(std::time::Duration::from_millis(100));
        },
        "attach" => {
            #[cfg(target_os = "linux")]
            // SAFETY: prctl with PR_SET_PTRACER (0x59616d61) and PR_SET_PTRACER_ANY (-1).
            unsafe {
                libc::prctl(0x5961_6d61, -1i64 as libc::c_ulong, 0, 0, 0);
            }
            loop {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
        "crash" => {
            println!("about to crash");
            let p = std::hint::black_box(std::ptr::null::<u64>());
            println!("{}", dbg_crash(p));
        }
        "echo" => {
            for line in std::io::stdin().lock().lines().map_while(Result::ok) {
                println!("echo: {line}");
            }
        }
        "thread" => {
            let t = std::thread::spawn(|| {
                for i in 0..5 {
                    DBG_COUNTER.fetch_add(dbg_add(i, 1), Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            });
            let _ = t.join();
        }
        _ => {
            for i in 0..5 {
                DBG_COUNTER.fetch_add(dbg_add(i, 1), Ordering::SeqCst);
            }
            println!("done {}", DBG_COUNTER.load(Ordering::SeqCst));
        }
    }
}
