use super::*;

impl App {
    pub(super) fn on_mail_sent(&mut self, id: String, to: String, to_name: String, summary: String) {
        let peer = if to_name.is_empty() { to } else { to_name };
        self.push(Block::Mail { id, outgoing: true, peer, text: summary, stage: nexo_core::mail::Stage::Sent });
    }

    pub(super) fn on_mail_failed(&mut self, to: String, reason: String) {
        self.notice(Level::Warn, format!("not sent to {to}: {reason}"));
        self.flash_level(Level::Warn, &format!("message not sent: {reason}"));
    }

    pub(super) fn on_mail_arrived(&mut self, id: String, from: String, from_name: String, summary: String, held: bool) {
        let peer = if from_name.is_empty() { from } else { from_name };
        let stage = if held { nexo_core::mail::Stage::Held } else { nexo_core::mail::Stage::Sent };
        self.push(Block::Mail { id, outgoing: false, peer: peer.clone(), text: summary, stage });
        self.flash(&format!("message from {peer}"));
    }

    pub(super) fn on_mail_delivered(&mut self, ids: Vec<String>) {
        for id in ids {
            self.mail_stage(&id, nexo_core::mail::Stage::Delivered);
        }
    }

    pub(super) fn on_mail_read(&mut self, ids: Vec<String>) {
        for id in ids {
            self.mail_stage(&id, nexo_core::mail::Stage::Read);
        }
    }

    pub(super) fn on_peers(&mut self, peers: Vec<nexo_core::mail::PeerInfo>) {
        self.peers = peers;
        self.dirty = true;
    }
}
