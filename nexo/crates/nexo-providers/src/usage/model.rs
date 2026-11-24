use serde_json::{json, Value};

/// The two plans that can be read: ChatGPT's (`codex`) and GitHub Copilot's.
pub const CODEX: &str = "codex";
pub const COPILOT: &str = "copilot";
pub const XAI: &str = "xai";
pub const CLAUDE: &str = "claude";
/// The kind of a reading made from the rate-limit headers of a host: `limits:api.openai.com`.
pub const LIMITS: &str = "limits:";

/// One gauge of a plan: a window of time (`5h`, `wk`) or a count of requests (`premium`).
#[derive(Debug, Clone, PartialEq)]
pub struct Meter {
    /// Where it sits in the plan (`primary`, `secondary`, `premium`, `chat`): a new reading of a slot
    /// replaces the old one, the other slots stay.
    pub slot: String,
    /// What the person reads: `5h`, `wk`, `premium`.
    pub label: String,
    /// 0 to 100 (more when over).
    pub used_pct: f64,
    /// Requests used and granted, when the plan counts them.
    pub counts: Option<(u64, u64)>,
    /// How long the window is.
    pub window_minutes: Option<u64>,
    /// When it starts over, unix seconds.
    pub resets_at: Option<i64>,
    /// Local time when this individual slot was observed, not a vendor timestamp.
    pub observed_at: Option<i64>,
}

impl Meter {
    /// The reading has been overtaken by its own reset: the window started over after it was taken.
    pub fn is_stale(&self, now: i64) -> bool {
        self.resets_at.is_some_and(|r| r <= now)
    }

    /// The share still free, 0 to 100.
    pub fn left_pct(&self) -> f64 {
        (100.0 - self.used_pct).clamp(0.0, 100.0)
    }

    /// What is left: `5h 63% left` or `premium 90/300 left`.
    pub fn short(&self) -> String {
        match self.counts {
            Some((used, total)) => format!("{} {}/{total} left", self.label, total.saturating_sub(used)),
            None => format!("{} {:.0}% left", self.label, self.left_pct()),
        }
    }

    /// Seconds to the reset, if it is known and still to come.
    pub fn resets_in(&self, now: i64) -> Option<i64> {
        self.resets_at.map(|r| r - now).filter(|s| *s > 0)
    }
}

/// What the plan of one login allows and how much of it is used.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlanUsage {
    /// `codex`, `copilot`, `xai` or `limits:{host}`.
    pub kind: String,
    /// The plan's name as the vendor gives it (`plus`, `pro`, `individual`).
    pub plan: Option<String>,
    pub meters: Vec<Meter>,
    /// ChatGPT credits beyond the plan: `unlimited` or the balance.
    pub credits: Option<String>,
    /// When it was read, unix seconds.
    pub seen: i64,
}

/// One piece of the status bar: its text and the share used (for the colour).
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub text: String,
    pub pct: f64,
}

impl PlanUsage {
    /// A reading made of the rate-limit headers of a host, not of a plan.
    pub fn is_limits(&self) -> bool {
        self.kind.starts_with(LIMITS)
    }

    /// The name of the plan or of the host's limits, as shown.
    pub fn name(&self) -> String {
        match self.kind.as_str() {
            CODEX => "ChatGPT".to_string(),
            COPILOT => "GitHub Copilot".to_string(),
            XAI => "SuperGrok".to_string(),
            CLAUDE => "Claude".to_string(),
            k => match k.strip_prefix(LIMITS) {
                Some(host) => format!("{host} rate limits"),
                None => k.to_string(),
            },
        }
    }

    /// The figures that matter on one line each: the windows still running, `5h 63% left ↻2h`. The reset
    /// is left out when `with_reset` is false.
    pub fn items(&self, now: i64, with_reset: bool) -> Vec<Item> {
        self.meters
            .iter()
            .filter(|m| !m.is_stale(now))
            .map(|m| {
                let mut text = m.short();
                if let (true, Some(s)) = (with_reset, m.resets_in(now)) {
                    text.push_str(&format!(" ↻{}", short_span(s)));
                }
                Item { text, pct: m.used_pct }
            })
            .collect()
    }

    /// Estimated seconds from now to exhaustion at the observed window-average pace.
    /// Match the weekly status line after three hours; keep the conservative margin for shorter windows.
    pub fn burn_eta(&self, meter: &Meter, now: i64) -> Option<i64> {
        let seen = meter.observed_at?;
        let age = now.checked_sub(seen)?;
        let reset = meter.resets_at?;
        if self.is_limits() || seen <= 0 || !(0..=3600).contains(&age) || reset <= now {
            return None;
        }
        let minutes = meter.window_minutes?;
        let window = i64::try_from(minutes.checked_mul(60)?).ok()?;
        let remaining = reset.checked_sub(seen)?;
        let elapsed = window.checked_sub(remaining)?;
        let used = meter.used_pct;
        if elapsed <= 0 || !(used > 0.0 && used < 100.0) {
            return None;
        }
        if minutes == 7 * 24 * 60 {
            if elapsed < 3 * 3600 {
                return None;
            }
        } else {
            let fraction = elapsed as f64 / window as f64;
            if fraction < 0.05 || used < 10.0 || used <= fraction * 125.0 {
                return None;
            }
        }
        let to_empty = elapsed as f64 * (100.0 - used) / used;
        (to_empty < remaining as f64 && to_empty > age as f64).then(|| (to_empty - age as f64).ceil() as i64)
    }

    /// Whether there is a current early-exhaustion estimate for this window.
    pub fn fast_pace(&self, meter: &Meter, now: i64) -> bool {
        self.burn_eta(meter, now).is_some()
    }

    /// The full detail, one line per window and a line for the plan and the credits.
    pub fn detail(&self, now: i64) -> Vec<String> {
        let mut out = Vec::new();
        let name = self.name();
        out.push(match &self.plan {
            Some(p) => format!("{name} · {p}"),
            None => name,
        });
        for m in &self.meters {
            let used = match m.counts {
                Some((u, t)) => format!("{} of {t} left ({u} used)", t.saturating_sub(u)),
                None => format!("{:.0}% left ({:.0}% used)", m.left_pct(), m.used_pct),
            };
            let span = m.window_minutes.map(|w| format!(" ({} window)", long_span(w as i64 * 60))).unwrap_or_default();
            let reset = match (m.resets_in(now), m.resets_at) {
                (Some(s), _) => format!(", resets in {}", long_span(s)),
                (None, Some(_)) => ", has reset since".to_string(),
                (None, None) => String::new(),
            };
            out.push(format!("{}{span}: {used}{reset}", m.label));
        }
        if let Some(c) = &self.credits {
            out.push(format!("credits: {c}"));
        }
        out
    }

    /// The figures as JSON, for scripts: `{"plan": "codex", "tier": "plus", "meters": [{"label": "5h", "used_percent": 37.0, "resets_in_seconds": 7200}]}`.
    pub fn to_json(&self, now: i64) -> Value {
        let meters: Vec<Value> = self
            .meters
            .iter()
            .map(|m| {
                let mut o = json!({"label": m.label, "used_percent": m.used_pct});
                if let Some((used, limit)) = m.counts {
                    o["used"] = json!(used);
                    o["limit"] = json!(limit);
                }
                if let Some(s) = m.resets_in(now) {
                    o["resets_in_seconds"] = json!(s);
                }
                o
            })
            .collect();
        let mut v = json!({"plan": self.kind, "meters": meters});
        if let Some(t) = &self.plan {
            v["tier"] = json!(t);
        }
        v
    }

    /// A newer reading on top of this one: its slots replace the old ones, the rest stay.
    pub(super) fn merge(&mut self, new: PlanUsage) {
        for m in new.meters {
            match self.meters.iter_mut().find(|o| o.slot == m.slot) {
                Some(o) => *o = m,
                None => self.meters.push(m),
            }
        }
        self.plan = new.plan.or(self.plan.take());
        self.credits = new.credits.or(self.credits.take());
        self.seen = new.seen;
        self.meters.sort_by_key(|m| slot_rank(&m.slot));
    }
}

pub(super) fn slot_rank(slot: &str) -> u8 {
    match slot {
        "primary" | "premium" => 0,
        "secondary" => 1,
        _ => 2,
    }
}

/// `2h`, `2h 14m`-ish in few characters: the two biggest units, the second dropped from a day on.
pub fn short_span(secs: i64) -> String {
    let m = (secs.max(0) + 30) / 60;
    match m {
        0 => "<1m".to_string(),
        m if m < 60 => format!("{m}m"),
        m if m < 60 * 24 => match m % 60 {
            0 => format!("{}h", m / 60),
            r => format!("{}h{r}m", m / 60),
        },
        m => match (m / 60) % 24 {
            0 => format!("{}d", m / 1440),
            r => format!("{}d{r}h", m / 1440),
        },
    }
}

/// `2h 14m`, `4d 3h`: for the detail.
pub fn long_span(secs: i64) -> String {
    short_span(secs).replace('h', "h ").replace('d', "d ").trim_end().to_string()
}

/// The name a window of `minutes` goes by: 300 is `5h`, 10080 is `wk`.
pub fn window_label(minutes: Option<u64>, fallback: &str) -> String {
    match minutes {
        Some(10_080) => "wk".to_string(),
        Some(m) if m > 0 && m % 1440 == 0 && m < 10_080 => format!("{}d", m / 1440),
        Some(m) if m > 0 && m % 60 == 0 && m < 10_080 => format!("{}h", m / 60),
        Some(m) if m > 0 => format!("{m}m"),
        _ => fallback.to_string(),
    }
}
