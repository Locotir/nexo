//! What the setup does with the answers that come back: logins, probes, model checks, and going back a step.

use super::*;

impl Setup {
    /// The list of the sign-in step: the saved login (when there is one), then the ways to sign in.
    pub(super) fn fill_login(&mut self) {
        self.signed = None;
        let flow = self.cfg.login.clone().unwrap_or_default();
        let mut items = Vec::new();
        if nexo_core::auth::get(self.cfg.auth.as_deref().unwrap_or(&self.name)).is_some() {
            items.push(PickItem {
                label: "Keep the saved login".into(),
                value: "keep".into(),
                hint: "already signed in on this machine".into(),
            });
        }
        items.extend(nexo_providers::login::choices(&flow).into_iter().map(|c| PickItem {
            label: c.label,
            value: c.flow,
            hint: c.hint,
        }));
        self.list = Picker::new(PickKind::Model, "", items, None);
        self.prompt = None;
        self.step = Step::Login;
    }

    /// The page a sign-in waits on arrived.
    pub fn login_prompt(&mut self, url: String, code: String) {
        if self.step == Step::Login && self.busy.is_some() {
            self.prompt = Some((url, code));
        }
    }

    /// A sign-in ended: on success the provider is asked what it serves.
    pub fn logged_in(&mut self, result: Result<(), String>) -> SetupOut {
        if self.step != Step::Login || self.busy.is_none() {
            return SetupOut::Nothing;
        }
        self.busy = None;
        self.prompt = None;
        match result {
            Ok(()) => {
                self.signed = Some(format!("signed in to {}", self.name));
                self.step = Step::Address;
                self.field = self.cfg.base_url.clone();
                self.probe_cmd()
            }
            Err(e) => {
                self.error = Some(e);
                SetupOut::Nothing
            }
        }
    }

    /// The answer to a probe of the address.
    pub fn probed(&mut self, result: Result<ServerProbe, String>) {
        if self.busy.is_none() {
            return;
        }
        self.busy = None;
        match result {
            Err(e) if self.cfg.login.is_some() => {
                self.fill_login();
                self.error = Some(e);
            }
            Err(e) => {
                let again = if self.step == Step::Address { Again::Address } else { Again::Probe };
                self.trouble(ProviderError::new(ErrorKind::Network, e), again);
            }
            Ok(p) => {
                self.cfg.protocol = p.protocol.clone();
                self.cfg.base_url = p.base_url.clone();
                if p.needs_key && p.models.is_empty() {
                    let refused = self.has_key();
                    self.probe = Some(p);
                    if refused {
                        // the key that was sent did not open it: nothing is kept
                        let e = ProviderError::new(ErrorKind::Auth, "the key was refused").with_status(401);
                        self.trouble(e, Again::Key);
                    } else {
                        self.field.clear();
                        self.step = Step::Key;
                    }
                    return;
                }
                let items: Vec<PickItem> = p
                    .models
                    .iter()
                    .map(|m| {
                        let mut hint = Vec::new();
                        if let Some(n) = m.context_window {
                            hint.push(format!("{} ctx", fmt_k(n as u64)));
                        }
                        if self.cfg.login.is_some() {
                            // a subscription: there is no per-token price to compare
                            hint.push("plan".into());
                        } else if let Some(pr) = m.pricing {
                            hint.push(format!("${} / ${}", fmt_price(pr.input), fmt_price(pr.output)));
                        }
                        if m.reasoning {
                            hint.push("reasons".into());
                        }
                        PickItem { label: m.id.clone(), value: m.id.clone(), hint: hint.join(" · ") }
                    })
                    .collect();
                let preferred = self.cfg.default_model.clone();
                self.list = Picker::new(PickKind::Model, "", items, preferred.as_deref());
                self.step = Step::Model;
                self.probe = Some(p);
            }
        }
    }

    /// The answer to the check of a model: the review, prefilled.
    pub fn checked(&mut self, check: ModelCheck) {
        if self.busy.is_none() {
            return;
        }
        self.busy = None;
        let i = &check.info;
        // the last word on a fact is the one that set it
        let src = |what: &str| {
            check
                .sources
                .iter()
                .rev()
                .find(|s| s.contains(what))
                .map(|s| s.split(" from ").nth(1).unwrap_or("").to_string())
                .unwrap_or_default()
        };
        let num = |n: Option<u32>| n.map(|n| n.to_string()).unwrap_or_default();
        let price = |f: fn(&Pricing) -> f64| i.pricing.map(|p| fmt_price(f(&p))).unwrap_or_default();
        let unknown = |v: &str, s: String| if v.is_empty() { "unknown: type it if you know it".to_string() } else { s };
        let ctx = num(i.context_window);
        let out = num(i.max_output);
        let (pi, po, pc) = (price(|p| p.input), price(|p| p.output), price(|p| p.cache_read));
        self.fields = vec![
            Field { label: "name", value: self.name.clone(), source: "how nexo calls this provider".into() },
            Field { label: "context window", source: unknown(&ctx, src("context")), value: ctx },
            Field { label: "largest reply", source: unknown(&out, src("largest reply")), value: out },
            Field { label: "price in", source: unknown(&pi, src("prices")), value: pi },
            Field { label: "price out", source: "per million tokens".into(), value: po },
            Field { label: "price cached", source: "per million tokens".into(), value: pc },
        ];
        self.efforts = i.efforts.clone();
        self.effort = None;
        self.selected = 0;
        self.details = false;
        let failed = check.error.clone();
        self.check = Some(check);
        self.step = Step::Review;
        // the request failed: why, in plain words; the facts stay behind it for `S`
        if let Some(e) = failed {
            self.trouble(error_of(&e), Again::Check);
        }
    }

    /// The configuration the review describes, ready to save.
    pub fn result(&self) -> (String, ProviderConfig) {
        let mut cfg = self.cfg.clone();
        let get = |label: &str| {
            self.fields.iter().find(|f| f.label == label).map(|f| f.value.trim().to_string()).unwrap_or_default()
        };
        let name = Some(get("name")).filter(|n| !n.is_empty()).unwrap_or_else(|| self.name.clone());
        let m = self.model.clone();
        if let Ok(n) = get("context window").replace('_', "").parse::<u32>() {
            cfg.context_window.insert(m.clone(), n);
        }
        if let Ok(n) = get("largest reply").replace('_', "").parse::<u32>() {
            cfg.max_output.insert(m.clone(), n);
        }
        let f = |label: &str| get(label).trim_start_matches('$').parse::<f64>().ok();
        if let (Some(input), Some(output)) = (f("price in"), f("price out")) {
            cfg.pricing.insert(
                m.clone(),
                Pricing { input, output, cache_read: f("price cached").unwrap_or(0.0), cache_write: 0.0 },
            );
        }
        if !self.efforts.is_empty() {
            cfg.efforts.insert(m.clone(), self.efforts.clone());
        }
        if let Some(e) = self.effort.and_then(|i| self.efforts.get(i)) {
            cfg.effort.insert(m.clone(), e.clone());
        }
        (name, cfg)
    }

    /// Back one step; `Close` from the first.
    pub(super) fn back(&mut self) -> SetupOut {
        self.error = None;
        if self.busy.take().is_some() {
            // a sign-in waiting for the browser is dropped
            if self.step == Step::Login {
                self.prompt = None;
                return SetupOut::Send(AgentCommand::CancelLogin);
            }
            // the running check is stopped, and whatever it answers later belongs to no one
            self.seq = next_seq();
            return SetupOut::Send(AgentCommand::CancelCheck);
        }
        match self.step {
            // text typed (hidden) on the first screen goes first; a second Esc closes
            Step::Start if !self.field.is_empty() => self.field.clear(),
            Step::Start | Step::Done => return SetupOut::Close,
            // a pasted text that was not taken for a key is dropped, and the person is told
            Step::Provider if self.pending_key.take().is_some() => {
                self.fill_providers();
                if self.from_start {
                    self.fill_start();
                }
                self.error = Some(HELD_DROPPED.into());
            }
            Step::Provider if self.from_start => self.fill_start(),
            Step::Provider => return SetupOut::Close,
            Step::Login | Step::Address | Step::Key => self.go_origin(),
            Step::Trouble if self.again == Again::Check => {
                self.problem = None;
                self.step = Step::Model;
            }
            Step::Trouble => self.go_origin(),
            Step::Model if self.cfg.login.is_some() => self.fill_login(),
            // the key was typed on the step before: it is asked for again, the old one is dropped
            Step::Model if self.key.is_some() && self.cfg.api_key_env.is_some() => {
                self.key = None;
                self.field.clear();
                self.step = Step::Key;
            }
            Step::Model => self.go_origin(),
            Step::Review => {
                self.check = None;
                self.step = Step::Model;
            }
        }
        SetupOut::Nothing
    }
}
