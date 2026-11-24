//! `/chrome`: the way into the person's own browser. The checks and the setup are `nexo_tools`'s (the same code
//! `nexo chrome-setup` runs); this answers the interface's commands with them, off the loop, so a browser that is
//! slow to answer holds nothing up.

use nexo_tools::browser::chrome_setup::{self, Places};

use super::*;

/// What this session's browser tool uses, if it is the person's own browser.
fn in_use(st: &State) -> Option<String> {
    st.tools.get("browser").and_then(|t| t.in_use())
}

/// Looks at everything and sends it as a `Chrome` event.
pub(super) async fn check(state: &Shared, events: &EventTx) {
    let used = in_use(&*state.lock().await);
    let events = events.clone();
    tokio::spawn(async move {
        match Places::user() {
            Ok(p) => emit(&events, AgentEvent::Chrome(chrome_setup::check(&p, used).await)),
            Err(e) => notice(&events, Level::Error, format!("/chrome: {e}")),
        }
    });
}

/// Does what `nexo chrome-setup` does, opens the extensions page and sends the new state with the steps.
pub(super) async fn set_up(state: &Shared, events: &EventTx) {
    let used = in_use(&*state.lock().await);
    let events = events.clone();
    tokio::spawn(async move {
        let places = match Places::user() {
            Ok(p) => p,
            Err(e) => return notice(&events, Level::Error, format!("/chrome: {e}")),
        };
        let p = places.clone();
        let done = match tokio::task::spawn_blocking(move || chrome_setup::setup(&p)).await {
            Ok(Ok(d)) => d,
            Ok(Err(e)) => return notice(&events, Level::Error, format!("/chrome: set up failed: {e}")),
            Err(e) => return notice(&events, Level::Error, format!("/chrome: set up failed: {e}")),
        };
        for note in &done.notes {
            notice(&events, Level::Warn, format!("/chrome: {note}"));
        }
        if cfg!(target_os = "macos") {
            let copied = chrome_setup::copy_folder_on_mac(&done.extension_dir);
            let tail = if copied { " The path is on your clipboard." } else { "" };
            notice(&events, Level::Info, format!("/chrome: {}{tail}", chrome_setup::MAC_FOLDER_HINT));
        }
        match chrome_setup::open_extensions(&done) {
            Ok(true) => {}
            Ok(false) => {
                notice(&events, Level::Info, "/chrome: no browser program found: open chrome://extensions yourself")
            }
            Err(e) => notice(&events, Level::Warn, format!("/chrome: {e}")),
        }
        let mut info = chrome_setup::check(&places, used).await;
        info.just_set_up = true;
        emit(&events, AgentEvent::Chrome(info));
    });
}

/// The note the model reads with the person's next request once they connected the browser `id`.
pub(super) fn use_note(id: &str) -> String {
    format!(
        "nexo note, not from the person's typing: they ran /chrome and connected their own browser (id {id}) \
         through the nexo extension. The `browser` tool's `select_browser{{\"id\":\"{id}\"}}` uses it; the extension \
         grants session access automatically. Cancel on Chrome's debugging bar stops that session."
    )
}

/// Readies [`use_note`] to go with the next request.
pub(super) async fn use_browser(state: &Shared, events: &EventTx, id: String) {
    let note = use_note(&id);
    let mut st = state.lock().await;
    if !st.hook_context.contains(&note) {
        st.hook_context.push(note);
    }
    notice(events, Level::Info, "the agent will be told your browser is connected with your next message");
}

#[cfg(test)]
mod tests {
    #[test]
    fn browser_note_describes_automatic_access_and_stop() {
        let note = super::use_note("test-browser");
        assert!(note.contains("select_browser{\"id\":\"test-browser\"}"));
        assert!(note.contains("grants session access automatically"));
        assert!(note.contains("Cancel on Chrome's debugging bar"));
        assert!(!note.contains("Allow"));
    }
}
