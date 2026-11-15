//! Process-wide values the binary sets once and the interface reads: the mailbox and the
//! supervision options.

static SUPERVISE: std::sync::OnceLock<nexo_agent::supervise::Options> = std::sync::OnceLock::new();
static MAILBOX: std::sync::OnceLock<std::sync::Arc<nexo_core::mailbox::Mailbox>> = std::sync::OnceLock::new();

/// The mailbox of this process: the interface polls it, hands its deliveries to the agent and shows what
/// it brings. Unset (as in tests, or with mail off), nothing is polled.
pub fn set_mailbox(m: std::sync::Arc<nexo_core::mailbox::Mailbox>) {
    let _ = MAILBOX.set(m);
}

pub(crate) fn mailbox() -> Option<std::sync::Arc<nexo_core::mailbox::Mailbox>> {
    MAILBOX.get().cloned()
}

/// Where `/panel` keeps its audit log and listens for hooks. Unset (as in tests), it writes nothing and
/// listens nowhere.
pub fn set_supervise_options(o: nexo_agent::supervise::Options) {
    let _ = SUPERVISE.set(o);
}

pub(crate) fn supervise_options() -> nexo_agent::supervise::Options {
    SUPERVISE.get().cloned().unwrap_or_default()
}
