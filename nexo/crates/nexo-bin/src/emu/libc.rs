//! Library calls by name. A name with no stub here returns `None` and the run stops on it.

use super::mem::Fault;
use super::{Machine, Stop};

type Answer = Result<(u64, String, Option<String>), Stop>;

/// Run the stub for `name`: the value for rax, the arguments as text and, when it reads better than hex,
/// the value as text. `None` when there is no stub.
pub(super) fn call(m: &mut Machine, name: &str) -> Option<Answer> {
    let lib = |call: &str, fault: Fault| Stop::LibFault { call: call.to_string(), fault };
    Some(match name {
        "exit" | "_exit" | "_Exit" => match m.arg(0) {
            Ok(c) => Err(Stop::Exit(c as i32 as i64)),
            Err(f) => Err(lib(name, f)),
        },
        "abort" => Err(Stop::Abort("the program called abort()".into())),
        "__stack_chk_fail" | "__chk_fail" => Err(Stop::Abort(
            "stack smashing: the canary at fs:0x28 changed before the function returned (__stack_chk_fail)".into(),
        )),
        "puts" => {
            let p = match m.arg(0) {
                Ok(p) => p,
                Err(f) => return Some(Err(lib(name, f))),
            };
            match m.mem.cstr(p, 1 << 16) {
                Ok((mut b, _)) => {
                    let shown = m.show_ptr(p);
                    b.push(b'\n');
                    m.emit(false, &b);
                    Ok((b.len() as u64, shown, None))
                }
                Err(f) => Err(lib("puts", f)),
            }
        }
        _ => return None,
    })
}
