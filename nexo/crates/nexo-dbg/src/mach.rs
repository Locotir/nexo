//! The Mach calls and records the macOS backend uses that the libc crate does not declare, written by hand from
//! the system headers (`mach/mach_vm.h`, `mach/thread_act.h`, `mach/task.h`, `mach/message.h`, `mach/exc.h`) so no
//! crate is added for them. Every number here is the one in those headers.

#![allow(non_camel_case_types)]

use libc::{c_int, mach_port_t};

pub type Port = mach_port_t;
pub type Kr = libc::kern_return_t;

pub const KERN_SUCCESS: Kr = 0;
pub const KERN_FAILURE: Kr = 5;

pub const MACH_PORT_NULL: Port = 0;
pub const MACH_PORT_RIGHT_RECEIVE: u32 = 1;
pub const MACH_MSG_TYPE_MAKE_SEND: u32 = 20;

pub const MACH_SEND_MSG: c_int = 0x1;
pub const MACH_RCV_MSG: c_int = 0x2;
pub const MACH_SEND_TIMEOUT: c_int = 0x10;
pub const MACH_RCV_TIMEOUT: c_int = 0x100;

pub const EXC_BAD_ACCESS: i32 = 1;
pub const EXC_BAD_INSTRUCTION: i32 = 2;
pub const EXC_ARITHMETIC: i32 = 3;
pub const EXC_SOFTWARE: i32 = 5;
pub const EXC_BREAKPOINT: i32 = 6;
/// `code[0]` of an `EXC_SOFTWARE` that is a Unix signal sent to a process traced with `PT_ATTACHEXC`.
pub const EXC_SOFT_SIGNAL: i64 = 0x10003;
/// `code[0]` of an `EXC_BREAKPOINT`: the trap flag (one instruction, or a debug register) and `int3`.
pub const EXC_I386_SGL: i64 = 1;
pub const EXC_I386_BPT: i64 = 2;
/// `code[0]` of an `EXC_BAD_ACCESS` the kernel turns into `SIGSEGV`; the others become `SIGBUS`.
pub const KERN_INVALID_ADDRESS: i64 = 1;
pub const EXC_I386_GPFLT: i64 = 13;

/// The exceptions the debugger takes, as an `exception_mask_t`.
pub const EXC_MASK: u32 =
    1 << EXC_BAD_ACCESS | 1 << EXC_BAD_INSTRUCTION | 1 << EXC_ARITHMETIC | 1 << EXC_SOFTWARE | 1 << EXC_BREAKPOINT;
pub const EXCEPTION_DEFAULT: c_int = 1;
pub const MACH_EXCEPTION_CODES: c_int = 0x8000_0000_u32 as c_int;
/// `THREAD_STATE_NONE` on x86: `EXCEPTION_DEFAULT` sends no thread state.
pub const THREAD_STATE_NONE: c_int = 13;
/// `EXC_TYPES_COUNT`: the most exception ports one swap hands back.
pub const EXC_TYPES_COUNT: usize = 14;
/// `mach_exception_raise`: the request `EXCEPTION_DEFAULT | MACH_EXCEPTION_CODES` sends.
pub const MACH_EXCEPTION_RAISE: i32 = 2405;

pub const X86_THREAD_STATE64: c_int = 4;
pub const X86_THREAD_STATE64_COUNT: u32 = 42;
pub const X86_DEBUG_STATE64: c_int = 11;
pub const X86_DEBUG_STATE64_COUNT: u32 = 16;
pub const X86_FLOAT_STATE64: c_int = 5;
pub const X86_FLOAT_STATE64_COUNT: u32 = 131;
pub const TASK_DYLD_INFO: u32 = 17;
pub const TASK_DYLD_INFO_COUNT: u32 = 5;

pub const VM_PROT_READ: c_int = 1;
pub const VM_PROT_WRITE: c_int = 2;
pub const VM_PROT_EXECUTE: c_int = 4;
pub const VM_PROT_COPY: c_int = 0x10;
pub const VM_REGION_SUBMAP_INFO_COUNT_64: u32 = 19;

/// `mach_msg_header_t`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct MsgHeader {
    pub bits: u32,
    pub size: u32,
    pub remote: Port,
    pub local: Port,
    pub voucher: Port,
    pub id: i32,
}

/// The reply to `mach_exception_raise` (`__Reply__mach_exception_raise_t`): the header, the NDR record and the
/// return code. A return code other than `KERN_SUCCESS` sends the exception on to the next handler, which for a
/// fault is the kernel's own that makes a Unix signal of it.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Reply {
    pub head: MsgHeader,
    pub ndr: [u8; 8],
    pub ret: Kr,
}

/// `NDR_record`: little-endian, ASCII, IEEE floats, as every Mac sends.
pub const NDR: [u8; 8] = [0, 0, 0, 0, 1, 0, 0, 0];

extern "C" {
    /// This process's own task port (`mach_task_self()` is a macro over it).
    pub static mach_task_self_: Port;
    pub fn mach_msg(
        msg: *mut MsgHeader,
        option: c_int,
        send_size: u32,
        rcv_size: u32,
        rcv_name: Port,
        timeout: u32,
        notify: Port,
    ) -> Kr;
    pub fn mach_port_allocate(task: Port, right: u32, name: *mut Port) -> Kr;
    pub fn mach_port_insert_right(task: Port, name: Port, poly: Port, poly_poly: u32) -> Kr;
    pub fn mach_port_deallocate(task: Port, name: Port) -> Kr;
    pub fn mach_port_mod_refs(task: Port, name: Port, right: u32, delta: c_int) -> Kr;
    pub fn task_suspend(task: Port) -> Kr;
    pub fn task_resume(task: Port) -> Kr;
    pub fn thread_suspend(thread: Port) -> Kr;
    pub fn thread_resume(thread: Port) -> Kr;
    pub fn thread_get_state(thread: Port, flavor: c_int, state: *mut u32, count: *mut u32) -> Kr;
    pub fn thread_set_state(thread: Port, flavor: c_int, state: *const u32, count: u32) -> Kr;
    pub fn task_set_state(task: Port, flavor: c_int, state: *const u32, count: u32) -> Kr;
    pub fn task_set_exception_ports(task: Port, mask: u32, port: Port, behavior: c_int, flavor: c_int) -> Kr;
    pub fn task_swap_exception_ports(
        task: Port,
        mask: u32,
        port: Port,
        behavior: c_int,
        flavor: c_int,
        masks: *mut u32,
        count: *mut u32,
        ports: *mut Port,
        behaviors: *mut c_int,
        flavors: *mut c_int,
    ) -> Kr;
    pub fn pid_for_task(task: Port, pid: *mut c_int) -> Kr;
    pub fn mach_vm_read_overwrite(task: Port, addr: u64, size: u64, data: u64, out: *mut u64) -> Kr;
    pub fn mach_vm_write(task: Port, addr: u64, data: usize, count: u32) -> Kr;
    pub fn mach_vm_protect(task: Port, addr: u64, size: u64, set_max: c_int, prot: c_int) -> Kr;
    pub fn mach_vm_region_recurse(
        task: Port,
        addr: *mut u64,
        size: *mut u64,
        depth: *mut u32,
        info: *mut u32,
        count: *mut u32,
    ) -> Kr;
}

/// What `mach_error_string` would say for the codes a debugger meets, so a refusal names its cause.
pub fn kr_text(kr: Kr) -> String {
    let name = match kr {
        1 => "KERN_INVALID_ADDRESS",
        2 => "KERN_PROTECTION_FAILURE",
        4 => "KERN_INVALID_ARGUMENT",
        5 => "KERN_FAILURE",
        15 => "KERN_INVALID_NAME",
        17 => "KERN_INVALID_RIGHT",
        37 => "KERN_TERMINATED",
        _ => return format!("Mach error {kr:#x}"),
    };
    format!("{name} ({kr})")
}
