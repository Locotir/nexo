//! The requests the person queued while a turn ran, when the next turn starts from them.

use std::collections::VecDeque;

use nexo_core::agent::Attachment;

use super::state::{InboxItem, State};

/// One request the person queued: its words, its files, and the header that says when it was typed.
type Queued = (String, Vec<Attachment>, String);

/// Everything queued, oldest first, as one request: the person wrote these one after another while the
/// model was busy, so the model reads them together and in the order they were written, in one request,
/// instead of answering each in a request of its own (two queued lines sent with Enter used to cost two
/// interrupted turns). When more than one has words, each comes under its header, the time it was typed and
/// whether the model was working then: without it the older line read as part of the newest one, and a line
/// typed before an Esc read as what the person wanted after it. One alone stays as it was typed. The queue is
/// left empty.
pub(super) fn take_all(inbox: &mut VecDeque<Queued>) -> Option<(String, Vec<Attachment>)> {
    if inbox.is_empty() {
        return None;
    }
    let mut attachments = Vec::new();
    let mut parts = Vec::new();
    for (text, files, header) in inbox.drain(..) {
        attachments.extend(files);
        if !text.trim().is_empty() {
            parts.push((text, header));
        }
    }
    let text = if parts.len() > 1 {
        let joined = parts.iter().map(|(text, header)| format!("{header}\n{text}")).collect::<Vec<_>>().join("\n\n");
        // the line before the first header says nexo wrote them: lines of the person's own that look like one stay theirs
        format!("{}\n{joined}", nexo_core::agent::JOINED_MARK)
    } else {
        parts.pop().map(|(text, _)| text).unwrap_or_default()
    };
    Some((text, attachments))
}

/// The header of a queued request's part in a joined one.
fn header_of(item: &InboxItem) -> String {
    nexo_core::agent::part_header(&nexo_core::clock::local_hms(item.at), item.during, item.stopped)
}

/// The next request the inbox holds and how many queued requests it joins: consecutive ordinary requests are
/// joined, but never across a directed receipt.
pub(super) fn take_queued(st: &mut State) -> (Option<InboxItem>, usize) {
    let Some(mut item) = st.pop_inbox() else { return (None, 0) };
    if item.directed.is_some() {
        return (Some(item), 1);
    }
    let mut parts = VecDeque::new();
    parts.push_back((std::mem::take(&mut item.text), std::mem::take(&mut item.attachments), header_of(&item)));
    let mut joined = 1;
    while st.inbox.front().is_some_and(|next| next.directed.is_none()) {
        let mut next = st.inbox.pop_front().expect("front exists");
        let header = header_of(&next);
        parts.push_back((std::mem::take(&mut next.text), std::mem::take(&mut next.attachments), header));
        joined += 1;
    }
    (item.text, item.attachments) = take_all(&mut parts).expect("one item exists");
    (Some(item), joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(lines: &[&str]) -> VecDeque<Queued> {
        lines.iter().enumerate().map(|(i, l)| (l.to_string(), Vec::new(), format!("[16:13:0{i}]"))).collect()
    }

    #[test]
    fn queued_requests_become_one_in_the_order_they_were_written_each_under_its_time() {
        let mut inbox = q(&["stop everything, thanks", "for now do nothing, I will tell you"]);
        let (text, _) = take_all(&mut inbox).unwrap();
        assert_eq!(text, "[messages typed one after another]\n[16:13:00]\nstop everything, thanks\n\n[16:13:01]\nfor now do nothing, I will tell you");
        assert!(inbox.is_empty());
    }

    #[test]
    fn one_queued_request_is_kept_as_it_was_and_an_empty_queue_gives_none() {
        let mut inbox = q(&["only this"]);
        assert_eq!(take_all(&mut inbox).unwrap().0, "only this");
        assert!(take_all(&mut inbox).is_none());
    }

    #[test]
    fn a_request_with_only_files_adds_no_blank_lines_and_no_header() {
        let mut inbox = q(&["", "look at this"]);
        assert_eq!(take_all(&mut inbox).unwrap().0, "look at this");
    }

    #[test]
    fn a_line_typed_before_an_esc_and_one_after_it_say_when_and_how_each_was_typed() {
        let mut before = InboxItem::sent("y documenta cada función".into(), Vec::new(), true);
        before.stopped = true;
        let after = InboxItem::sent("Detente, cambiamos de tema".into(), Vec::new(), false);
        let (h1, h2) = (header_of(&before), header_of(&after));
        assert!(h1.ends_with(", typed while you worked on the request above, before the person stopped it]"), "{h1}");
        assert!(nexo_core::agent::is_part_header(&h2) && !h2.contains("typed"), "{h2}");
        let mut inbox = VecDeque::from([
            (before.text.clone(), Vec::new(), h1.clone()),
            (after.text.clone(), Vec::new(), h2.clone()),
        ]);
        let (text, _) = take_all(&mut inbox).unwrap();
        assert_eq!(
            text,
            format!(
                "{}\n{h1}\ny documenta cada función\n\n{h2}\nDetente, cambiamos de tema",
                nexo_core::agent::JOINED_MARK
            )
        );
        assert_eq!(
            nexo_core::agent::joined_parts(&text),
            Some(vec!["y documenta cada función", "Detente, cambiamos de tema"])
        );
    }
}
