//! The lists of the setup: which rows each step shows, folded and filtered.

use super::*;

impl Setup {
    /// The first list: one fold per provider that is ready (signed in, key found, or a local
    /// server running), then one fold of the providers still to set up, then the row for a
    /// server of one's own. A fold shows its rows only when open (or while filtering).
    pub(super) fn fill_providers(&mut self) {
        self.signed = None;
        let in_use = self.current.split_once('/').map(|(p, _)| p.to_string());
        let count = |name: &str| self.models.iter().filter(|(p, _)| p == name).count();
        // key and sign-in facts of a provider: (ready, what to say about it, hint of its set-up row)
        let facts = |name: &str, c: &ProviderConfig| {
            let running = self.local.iter().find(|(p, _)| p == name).map(|(_, w)| w.clone());
            // a key kept with `nexo auth login` counts too; a command is not run just to list
            let stored = || nexo_core::auth::get(c.auth.as_deref().unwrap_or(name)).is_some();
            let (key, ok) = match &c.api_key_env {
                _ if c.login.is_some() => {
                    if stored() {
                        ("signed in ✓", true)
                    } else {
                        ("sign in", false)
                    }
                }
                _ if c.api_key_cmd.is_some() || stored() => ("key ✓", true),
                Some(v) if std::env::var(v).is_ok_and(|k| !k.trim().is_empty()) => ("key ✓", true),
                _ => ("", false),
            };
            let need = match &c.api_key_env {
                _ if !key.is_empty() => key.to_string(),
                Some(_) if c.login.is_none() => "needs a key".to_string(),
                _ => String::new(),
            };
            // what the person must do comes first, and no address unless it is all there is
            let hint = match (&running, need.is_empty()) {
                (Some(w), true) => format!("● running · {w}"),
                (Some(w), false) => format!("● running · {w} · {need}"),
                (None, true) if LOCAL.iter().any(|(l, _)| *l == name) => "on this computer".to_string(),
                (None, true) => c.base_url.clone(),
                // the address is for the one who chose to type it: a person is told what to do
                (None, false) => need.clone(),
            };
            let status = if running.is_some() {
                "running"
            } else if c.login.is_some() && ok {
                "signed in"
            } else if ok {
                "key found"
            } else {
                "configured"
            };
            (provider_ready(name, c, &self.local), status, hint)
        };
        // the providers with a model of their own first (the one in use at the top), then the
        // other ready ones, running servers before the rest
        let mut order: Vec<String> = Vec::new();
        for (p, _) in &self.models {
            if !order.contains(p) {
                order.push(p.clone());
            }
        }
        if let Some(pos) = in_use.as_ref().and_then(|u| order.iter().position(|p| p == u)) {
            let p = order.remove(pos);
            order.insert(0, p);
        }
        let mut extra: Vec<(bool, String)> = self
            .providers
            .iter()
            .filter(|(n, c)| !order.contains(n) && facts(n, c).0)
            .map(|(n, _)| (!self.local.iter().any(|(l, _)| l == n), n.clone()))
            .collect();
        extra.sort_by_key(|(not_running, _)| *not_running);
        order.extend(extra.into_iter().map(|(_, n)| n));

        let mut groups: Vec<Group> = Vec::new();
        for name in &order {
            let cfg = self.providers.iter().find(|(n, _)| n == name).map(|(_, c)| c);
            let status = match self.local.iter().find(|(p, _)| p == name) {
                Some((_, w)) => format!("● running · {w}"),
                None => cfg.map_or("configured", |c| facts(name, c).1).to_string(),
            };
            let n = count(name);
            let used = in_use.as_deref() == Some(name.as_str());
            let mut parts: Vec<String> = Vec::new();
            if n > 0 {
                parts.push(format!("{n} model{}", if n == 1 { "" } else { "s" }));
            }
            parts.push(if used { "● in use".to_string() } else { status });
            let mut rows: Vec<(PickItem, String)> = self
                .models
                .iter()
                .filter(|(p, _)| p == name)
                .map(|(p, m)| {
                    let v = format!("{p}/{m}");
                    let hint = if v == self.current { "● in use" } else { "switch" };
                    (PickItem { label: format!("   {m}"), value: format!("use:{v}"), hint: hint.into() }, v)
                })
                .collect();
            if let Some(c) = cfg {
                let again = if c.login.is_some() { "sign in again" } else { "set up again" };
                rows.push((
                    PickItem { label: format!("   {again}"), value: format!("new:{name}"), hint: facts(name, c).2 },
                    format!("set up {name} {again}"),
                ));
            }
            groups.push(Group { key: name.clone(), label: name.clone(), hint: parts.join(" · "), rows });
        }
        let rest: Vec<(PickItem, String)> = self
            .providers
            .iter()
            .filter(|(n, _)| !order.contains(n))
            .map(|(n, c)| {
                (
                    PickItem { label: format!("   set up {n}"), value: format!("new:{n}"), hint: facts(n, c).2 },
                    format!("set up {n}"),
                )
            })
            .collect();
        if !rest.is_empty() {
            let hint = format!("{} provider{}", rest.len(), if rest.len() == 1 { "" } else { "s" });
            groups.push(Group { key: OTHERS.into(), label: "others".into(), hint, rows: rest });
        }
        self.groups = groups;
        let keep = format!("use:{}", self.current);
        self.rebuild(Some(&keep));
    }

    /// The visible rows of the first list from the folds: the open ones show their rows, and
    /// while something is typed every fold with a match is open and shows only what matches.
    pub(super) fn rebuild(&mut self, keep: Option<&str>) {
        let q = self.list.query.trim().to_string();
        let filtering = !q.is_empty();
        let flat: Vec<(usize, usize)> =
            self.groups.iter().enumerate().flat_map(|(g, grp)| (0..grp.rows.len()).map(move |r| (g, r))).collect();
        let searches: Vec<String> = flat.iter().map(|&(g, r)| self.groups[g].rows[r].1.clone()).collect();
        let order = fuzzy::rank(&q, &searches);
        let mut items: Vec<PickItem> = Vec::new();
        let mut owner: Vec<Option<usize>> = Vec::new();
        for (g, grp) in self.groups.iter().enumerate() {
            let kids: Vec<usize> = order.iter().filter(|&&f| flat[f].0 == g).map(|&f| flat[f].1).collect();
            if filtering && kids.is_empty() {
                continue;
            }
            let open = filtering || self.open.contains(&grp.key);
            items.push(PickItem {
                label: format!("{} {}", if open { "▾" } else { "▸" }, grp.label),
                value: format!("group:{}", grp.key),
                hint: grp.hint.clone(),
            });
            owner.push(Some(g));
            if open {
                for r in kids {
                    items.push(grp.rows[r].0.clone());
                    owner.push(Some(g));
                }
            }
        }
        let own = PickItem { label: "set up your own server".into(), value: CUSTOM.into(), hint: "own server".into() };
        if !filtering || !fuzzy::rank(&q, &[format!("{} {}", own.label, own.hint)]).is_empty() {
            items.push(own);
            owner.push(None);
        }
        // typing lands on the best match, so Enter picks it; else on the row asked to keep
        let want = if filtering {
            order.first().map(|&f| self.groups[flat[f].0].rows[flat[f].1].0.value.clone())
        } else {
            keep.map(str::to_string)
        };
        self.list.selected = want.and_then(|v| items.iter().position(|i| i.value == v)).unwrap_or(0);
        self.list.ranked = (0..items.len()).collect();
        self.list.items = items;
        self.owner = owner;
    }

    /// Opens or closes the fold of the row under the cursor (the fold a model or set-up row is in).
    pub(super) fn fold(&mut self, open: bool) {
        if self.step != Step::Provider || !self.list.query.trim().is_empty() {
            return;
        }
        let Some(g) = self.owner.get(self.list.selected).copied().flatten() else { return };
        let on_header = self.list.current().is_some_and(|i| i.value.starts_with("group:"));
        let key = self.groups[g].key.clone();
        if open {
            if !on_header {
                return;
            }
            self.open.insert(key.clone());
        } else {
            self.open.remove(&key);
        }
        self.rebuild(Some(&format!("group:{key}")));
    }

    /// The first list is rebuilt from its folds; the others are filtered as they are.
    pub(super) fn refilter(&mut self) {
        if self.step == Step::Provider {
            self.rebuild(None);
        } else {
            self.list.refilter();
        }
    }

    /// How many rows the first list would have with every fold open: the room the dialog asks
    /// for, so what is opened later is not cut.
    pub fn rows_if_open(&self) -> usize {
        self.groups.iter().map(|g| 1 + g.rows.len()).sum::<usize>() + 1
    }

    /// The local servers found running; they become folds of their own.
    pub fn found_local(&mut self, found: Vec<(String, String)>) {
        self.local = found;
        self.scanned = true;
        if self.step == Step::Start {
            self.fill_start();
        } else if self.step == Step::Provider {
            let sel = self.list.current().map(|i| i.value.clone());
            self.fill_providers();
            if let Some(v) = sel {
                if let Some(pos) = self.list.items.iter().position(|i| i.value == v) {
                    self.list.selected = pos;
                }
            }
        }
    }

    pub(super) fn probe_cmd(&mut self) -> SetupOut {
        self.error = None;
        self.busy = Some(self.wait_text());
        self.seq = next_seq();
        SetupOut::Send(AgentCommand::Probe {
            name: self.name.clone(),
            cfg: self.cfg.clone(),
            key: self.key.clone(),
            seq: self.seq,
        })
    }

    /// The answer to a probe, applied only when it belongs to the probe that is open now.
    pub fn probed_from(&mut self, name: &str, seq: u64, result: Result<ServerProbe, String>) {
        if name == self.name && seq == self.seq && self.step != Step::Done {
            self.probed(result);
        }
    }

    /// The answer to a check of a model, applied only when it belongs to the check that is open now.
    pub fn checked_from(&mut self, name: &str, model: &str, seq: u64, check: ModelCheck) {
        if name == self.name && model == self.model && seq == self.seq {
            self.checked(check);
        }
    }
}
