//! Remote sessions seen from the sub-agent view: opening, polling, sending and merging snapshots.

use super::*;

impl App {
    /// Open a remote session without pretending it is a local sub-agent.
    #[cfg(unix)]
    pub(in crate::app) fn open_remote(&mut self, id: &str) {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let target = id.to_owned();
        tokio::spawn(async move {
            let mut cursor = None;
            loop {
                use nexo_agent::observe_remote::ObservationUpdate;
                let result = nexo_agent::observe_remote::snapshot_since(
                    &nexo_agent::observe_remote::default_root(),
                    &target,
                    cursor.as_ref(),
                )
                .await
                .map(|update| match update {
                    ObservationUpdate::Initial(s) => RemoteUpdate::Initial(s),
                    ObservationUpdate::Delta(s) => RemoteUpdate::Delta(s),
                    ObservationUpdate::Resync(s) => RemoteUpdate::Resync(s),
                })
                .map_err(|e| e.to_string());
                let next = result.as_ref().ok().map(|u| match u {
                    RemoteUpdate::Initial(s) | RemoteUpdate::Delta(s) | RemoteUpdate::Resync(s) => {
                        let mut c = s.clone();
                        c.events.clear();
                        c
                    }
                });
                // Never advance the cursor over an update that the UI has not received.
                let mut message = result;
                loop {
                    match tx.try_send(message) {
                        Ok(()) => break,
                        Err(std::sync::mpsc::TrySendError::Full(m)) => message = m,
                        Err(std::sync::mpsc::TrySendError::Disconnected(_)) => return,
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                if let Some(next) = next {
                    cursor = Some(next);
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        });
        self.open_remote_receiver(id, rx);
    }

    #[cfg(any(unix, test))]
    pub(in crate::app) fn open_remote_receiver(
        &mut self,
        id: &str,
        rx: std::sync::mpsc::Receiver<Result<RemoteUpdate, String>>,
    ) {
        self.close_sub();
        if self.sub_view.is_some() {
            return;
        }
        self.palette = None;
        self.suggestion = None;
        let draft = self.input.take_draft();
        self.sub_view = Some(SubView {
            id: id.to_owned(),
            remote: Some(RemoteView {
                rx,
                snapshot: None,
                resynced: false,
                error: None,
                target: None,
                delivery: RemoteDelivery::Ready,
                identity: None,
                invalid: false,
                close_requested: false,
                abandon_unknown_confirm: false,
            }),
            parked: Parked::empty(),
            main_in: true,
            draft: Some(draft),
            panel: None,
            pending: 0,
            answered: false,
            base: 0,
            grown: 0,
            asking: None,
            last_ask: None,
            again: false,
        });
        self.swap_fields();
        self.push(Block::Notice { level: Level::Info, text: "Connecting to remote observation…".into() });
        self.dirty = true;
    }

    #[cfg(any(unix, test))]
    pub(super) fn remote_tick(&mut self) {
        self.remote_delivery_tick();
        let result = self.sub_view.as_ref().and_then(|v| v.remote.as_ref()).and_then(|r| match r.rx.try_recv() {
            Ok(result) => Some(result),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err("observation worker stopped".into())),
        });
        let Some(result) = result else { return };
        match result {
            Err(error) => {
                let r = self.sub_view.as_mut().unwrap().remote.as_mut().unwrap();
                r.target = None;
                if r.error.as_ref() != Some(&error) {
                    r.error = Some(error.clone());
                    self.push(Block::Notice {
                        level: Level::Info,
                        text: format!("Remote observation disconnected: {}", safe_tool_text(&error)),
                    });
                    self.dirty = true;
                }
            }
            Ok(update) => {
                let view = self.sub_view.as_mut().unwrap();
                let r = view.remote.as_mut().unwrap();
                let snapshot = match &update {
                    RemoteUpdate::Initial(s) | RemoteUpdate::Delta(s) | RemoteUpdate::Resync(s) => s,
                };
                let target = snapshot.session_id.as_ref().filter(|id| **id == view.id).map(|id| {
                    nexo_core::agent::ObservationTarget {
                        instance_id: snapshot.instance_id.clone(),
                        session_id: id.clone(),
                        generation: snapshot.generation,
                    }
                });
                if target.is_none() || r.identity.as_ref().is_some_and(|old| target.as_ref() != Some(old)) {
                    r.invalid = true;
                }
                if r.invalid {
                    r.target = None;
                    if r.error.as_deref() != Some("identity changed") {
                        r.error = Some("identity changed".into());
                        self.push(Block::Notice {
                            level: Level::Info,
                            text: "Remote identity changed · close and reopen observation before sending.".into(),
                        });
                        self.dirty = true;
                    }
                    return;
                }
                if r.identity.is_none() {
                    r.identity = target.clone();
                }
                // An empty poll still needs validation, but should not rebuild the
                // transcript or append another notice when nothing has changed.
                let unchanged = matches!(&update, RemoteUpdate::Delta(next) if r.snapshot.as_ref().is_some_and(|old|
                    next.events.is_empty() && next.sequence == old.sequence && next.dropped == old.dropped
                    && next.permission_waiting == old.permission_waiting && next.question_waiting == old.question_waiting));
                let recovering = r.error.is_some();
                let result = merge_remote(&mut r.snapshot, update);
                if let Err(error) = result {
                    r.target = None;
                    r.invalid = true;
                    self.push(Block::Notice {
                        level: Level::Info,
                        text: format!("Remote observation invalid: {error} · close and reopen."),
                    });
                    self.dirty = true;
                    return;
                }
                r.resynced |= result.unwrap();
                r.target = target;
                r.error = None;
                if unchanged && !recovering {
                    return;
                }
                let (items, partial) = remote_items(r.snapshot.as_ref().unwrap());
                let resynced = r.resynced;
                let id = view.id.clone();
                self.sub_answer(&id, Some(SubTranscript { items, dropped: 0, partial }));
                self.push(Block::Notice {
                    level: Level::Info,
                    text: if resynced {
                        "Remote observation resynchronized after loss · earlier events may be missing."
                    } else {
                        "Remote live observation is bounded and may omit events."
                    }
                    .into(),
                });
            }
        }
    }

    #[cfg(any(unix, test))]
    pub(super) fn remote_delivery_tick(&mut self) {
        let Some(r) = self.sub_view.as_mut().and_then(|v| v.remote.as_mut()) else { return };
        let (text, request_id, result) = match &r.delivery {
            RemoteDelivery::Sending { text, request_id, rx } => match rx.try_recv() {
                Ok(result) => (text.clone(), request_id.clone(), result),
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    (text.clone(), request_id.clone(), RemoteReceipt::Unknown)
                }
            },
            _ => return,
        };
        r.delivery = if matches!(result, RemoteReceipt::Unknown) {
            RemoteDelivery::Unknown { text: text.clone(), request_id }
        } else {
            RemoteDelivery::Ready
        };
        if matches!(result, RemoteReceipt::Rejected(_) | RemoteReceipt::BeforeSend(_)) {
            let current = self.input.full_text();
            self.input.clear();
            self.input.set_text(&recover_rejected(&current, &text));
        }
        match result {
            RemoteReceipt::Accepted => {
                self.input.push_history(&text);
                self.toast(Kind::Info, "remote message consumed · response not yet complete");
            }
            RemoteReceipt::Rejected(reason) => {
                self.toast(Kind::Error, &format!("remote rejected: {} · draft kept", safe_tool_text(&reason)))
            }
            RemoteReceipt::BeforeSend(error) => {
                self.toast(Kind::Error, &format!("remote not sent: {} · draft kept", safe_tool_text(&error)))
            }
            RemoteReceipt::Unknown => {
                self.toast(Kind::Error, "remote delivery unknown · do not resend; delivery remains locked")
            }
        }
        self.dirty = true;
        let close = self
            .sub_view
            .as_ref()
            .and_then(|v| v.remote.as_ref())
            .is_some_and(|r| r.close_requested && matches!(r.delivery, RemoteDelivery::Ready));
        if close {
            let draft = self.input.full_text();
            if !draft.is_empty() {
                self.input.push_history(&draft);
            }
            self.close_sub();
            if !draft.is_empty() {
                self.toast(Kind::Info, "remote draft saved in input history");
            }
        }
    }

    #[cfg(any(unix, test))]
    pub(super) fn submit_remote(&mut self, text: String) {
        let Some(r) = self.sub_view.as_mut().and_then(|v| v.remote.as_mut()) else { return };
        match &r.delivery {
            RemoteDelivery::Sending { .. } => {
                self.toast(Kind::Info, "remote delivery pending");
                return;
            }
            RemoteDelivery::Unknown { text: uncertain, request_id } => {
                let _ = (uncertain, request_id); // No further send until explicit discard, even after editing.
                self.toast(Kind::Error, "delivery unknown · do not resend; delivery remains locked");
                return;
            }
            RemoteDelivery::Ready => {}
        }
        let Some(target) = r.target.clone() else {
            self.toast(Kind::Error, "remote observation unavailable · draft kept");
            return;
        };
        #[cfg(unix)]
        {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            let request_id = uuid::Uuid::new_v4().to_string();
            r.delivery = RemoteDelivery::Sending { text: text.clone(), request_id: request_id.clone(), rx };
            self.input.clear();
            tokio::spawn(async move {
                use nexo_agent::observe_remote::{intervene, InterventionResult};
                let result =
                    match intervene(&nexo_agent::observe_remote::default_root(), target, request_id, text).await {
                        Ok(InterventionResult::Accepted) => RemoteReceipt::Accepted,
                        Ok(InterventionResult::Rejected(reason)) => RemoteReceipt::Rejected(reason),
                        Ok(InterventionResult::Unknown) => RemoteReceipt::Unknown,
                        Err(error) => RemoteReceipt::BeforeSend(error.to_string()),
                    };
                let _ = tx.send(result);
            });
            self.toast(Kind::Info, "sending remote message · awaiting consumption");
            self.dirty = true;
        }
        #[cfg(not(unix))]
        {
            let _ = (target, text);
            self.toast(Kind::Error, "remote intervention unavailable · draft kept");
        }
    }
}

// Merge only contiguous, identity-consistent deltas; retain at most the host's bounded suffix.
#[cfg(any(unix, test))]
pub(super) fn merge_remote(
    slot: &mut Option<nexo_core::agent::ObservationSnapshot>,
    update: RemoteUpdate,
) -> Result<bool, &'static str> {
    let (next, kind) = match update {
        RemoteUpdate::Initial(s) => (s, 0),
        RemoteUpdate::Delta(s) => (s, 1),
        RemoteUpdate::Resync(s) => (s, 2),
    };
    if kind == 1 {
        let old = slot.as_mut().ok_or("delta before initial snapshot")?;
        if old.instance_id != next.instance_id
            || old.session_id != next.session_id
            || old.generation != next.generation
            || next.sequence < old.sequence
            || next.dropped < old.dropped
            || next.dropped > old.sequence
            || next.events.len() as u64 != next.sequence - old.sequence
            || next.events.iter().enumerate().any(|(i, e)| e.sequence != old.sequence + i as u64 + 1)
        {
            return Err("noncontiguous delta");
        }
        old.events.retain(|e| e.sequence > next.dropped);
        old.events.extend(next.events);
        old.sequence = next.sequence;
        old.dropped = next.dropped;
        old.permission_waiting = next.permission_waiting;
        old.question_waiting = next.question_waiting;
        Ok(false)
    } else {
        if (kind == 0) != slot.is_none() {
            return Err("unexpected snapshot");
        }
        if next.dropped > next.sequence
            || next.events.len() as u64 != next.sequence - next.dropped
            || next.events.iter().enumerate().any(|(i, e)| e.sequence != next.dropped + i as u64 + 1)
        {
            return Err("invalid snapshot sequence");
        }
        if let Some(old) = slot.as_ref() {
            if old.instance_id != next.instance_id
                || old.session_id != next.session_id
                || old.generation != next.generation
                || next.sequence < old.sequence
            {
                return Err("invalid resynchronization");
            }
        }
        *slot = Some(next);
        Ok(kind == 2)
    }
}

/// Keep later edits intact; a draft that starts with the sent text already
/// contains the whole rejected message, including when it has been extended.
pub(super) fn recover_rejected(draft: &str, sent: &str) -> String {
    if draft.starts_with(sent) {
        draft.to_owned()
    } else if draft.is_empty() {
        sent.to_owned()
    } else {
        format!("{draft}\n\n{sent}")
    }
}
