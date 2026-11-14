//! The requests the person queued while a turn ran, when the next turn starts from them.

use std::collections::VecDeque;

use nexo_core::agent::Attachment;

/// Everything queued, oldest first, as one request: the person wrote these one after another while the
/// model was busy, so the model reads them together and in the order they were written, in one request,
/// instead of answering each in a request of its own (two queued lines sent with Enter used to cost two
/// interrupted turns). The queue is left empty.
pub(super) fn take_all(inbox: &mut VecDeque<(String, Vec<Attachment>)>) -> Option<(String, Vec<Attachment>)> {
    let mut parts = inbox.drain(..);
    let (mut text, mut attachments) = parts.next()?;
    for (more, files) in parts {
        if !more.trim().is_empty() {
            if !text.trim().is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&more);
        }
        attachments.extend(files);
    }
    Some((text, attachments))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(lines: &[&str]) -> VecDeque<(String, Vec<Attachment>)> {
        lines.iter().map(|l| (l.to_string(), Vec::new())).collect()
    }

    #[test]
    fn queued_requests_become_one_in_the_order_they_were_written() {
        let mut inbox = q(&["stop everything, thanks", "for now do nothing, I will tell you"]);
        let (text, _) = take_all(&mut inbox).unwrap();
        assert_eq!(text, "stop everything, thanks\n\nfor now do nothing, I will tell you");
        assert!(inbox.is_empty());
    }

    #[test]
    fn one_queued_request_is_kept_as_it_was_and_an_empty_queue_gives_none() {
        let mut inbox = q(&["only this"]);
        assert_eq!(take_all(&mut inbox).unwrap().0, "only this");
        assert!(take_all(&mut inbox).is_none());
    }

    #[test]
    fn a_request_with_only_files_adds_no_blank_lines() {
        let mut inbox = q(&["", "look at this"]);
        assert_eq!(take_all(&mut inbox).unwrap().0, "look at this");
    }
}
