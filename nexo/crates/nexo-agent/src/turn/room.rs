//! Part of the agent loop; see the parent module.
//!
//! What one thing the session adds to a request besides the conversation may take of a small window: a tool's
//! result, a hook's text, the note of jobs that ended, and the files nexo attaches by itself. Each of them has a
//! cap of its own that ignores the window; here they are held to what the window leaves.

use super::*;

/// The share of the room the window leaves for conversation (after the fixed text and the reply) that one result
/// may take: a quarter, so four results and the words around them fit. An 8 000-token window asks 2 000 for the
/// reply and has 1 912 tokens of fixed text (measured on a bare session), which leaves about 4 000 for the
/// conversation, so one result may take about a thousand of them.
const SHARE: u64 = 4;

/// The least characters one result may carry however small the window is: less says nothing a model can use.
const FLOOR_CHARS: usize = 2_000;

/// Characters of one result in the room the window leaves, with the window it was worked out for. None when the
/// model's window is not known and no request was refused without a number, or its reply takes all of it. The room
/// is under the view's ceiling (`view_ceiling`), so a request refused without a number lowers it, whether the window
/// is known or not. The limit the cut line names is the model's window, or the ceiling when that is the smaller.
pub(super) fn result_room(st: &State) -> Option<nexo_core::ResultRoom> {
    let view = view_ceiling(st)?;
    let left = view.saturating_sub(fixed_tokens(&st.system, &offered(st)));
    // a token is four characters, and a result may take a quarter of what is left
    let chars = usize::try_from(left * 4 / SHARE).unwrap_or(usize::MAX).max(FLOOR_CHARS);
    let window = match st.limits.context_window {
        Some(w) if window_ceiling(st) == Some(view) => u64::from(w),
        _ => view,
    };
    Some(nexo_core::ResultRoom { chars, window })
}

/// `text` held to `room`: the head, and the fact of what was kept (`rest` says how to get the rest). What
/// fits is returned as it is.
pub(super) fn held(text: String, room: Option<nexo_core::ResultRoom>, what: &str, rest: &str) -> String {
    let Some(room) = room else { return text };
    let whole = text.chars().count();
    if whole <= room.chars {
        return text;
    }
    let kept = room.cap(usize::MAX);
    let head: String = text.chars().take(kept).collect();
    format!("{head}\n[{what} cut to {kept} of {whole} characters for a {}-token window; {rest}]", room.window)
}

/// The tokens the first call of a request stays under with the files nexo attaches by itself: [`orient::FIRST_CALL`],
/// or the view's ceiling (`view_ceiling`: the window, and a request refused without a number) when that is less.
pub(super) fn first_call(st: &State) -> u64 {
    view_ceiling(st).map_or(orient::FIRST_CALL, |c| orient::FIRST_CALL.min(c))
}

/// The note of jobs that ended, each report held to the room one result may take.
pub(super) fn ended_note(st: &State, ended: &[nexo_core::jobs::JobInfo]) -> String {
    st.jobs.note_ended_within(ended, result_room(st))
}

/// The note of the jobs that ended, taken from `jobs` now, for a turn the loop starts by itself.
pub(super) async fn ended_note_now(state: &Shared, jobs: &Jobs) -> String {
    let st = state.lock().await;
    jobs.note_ended_within(&jobs.take_ended(), result_room(&st))
}
