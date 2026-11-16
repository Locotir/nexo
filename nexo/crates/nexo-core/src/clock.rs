//! The local day, as files key it. The UTC offset is read once at start and shared by everything that
//! names a day (spend, session scans, the team table, the supervisor's spend cap).

/// The machine's UTC offset, read once. Reading it needs a single-threaded process, so
/// [`init_local_offset`] runs first thing in `main`, before the async runtime starts.
static OFFSET: std::sync::OnceLock<Option<time::UtcOffset>> = std::sync::OnceLock::new();

/// Reads the local UTC offset while the process still has one thread. Without it the day
/// rolls over at UTC midnight.
pub fn init_local_offset() {
    let _ = OFFSET.set(time::UtcOffset::current_local_offset().ok());
}

/// The local UTC offset read at start; None when it could not be read (UTC is used then).
pub fn local_offset() -> Option<time::UtcOffset> {
    OFFSET.get().copied().flatten()
}

/// Today, as the file keys it (`2026-10-28`, local time where known).
pub fn today() -> String {
    let utc = time::OffsetDateTime::now_utc();
    let now = match OFFSET.get().copied().flatten() {
        Some(o) => utc.to_offset(o),
        None => utc,
    };
    format!("{:04}-{:02}-{:02}", now.year(), u8::from(now.month()), now.day())
}

/// The time of `t` as `HH:MM:SS` on the local clock (UTC where the offset could not be read): what nexo writes when
/// it tells the model when something happened, as the person's own clock shows it.
pub fn local_hms(t: std::time::SystemTime) -> String {
    let utc = time::OffsetDateTime::from(t);
    let at = match local_offset() {
        Some(o) => utc.to_offset(o),
        None => utc,
    };
    format!("{:02}:{:02}:{:02}", at.hour(), at.minute(), at.second())
}
