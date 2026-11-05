//! The cheap model: asked only about an unanswered question or an unclear error loop, never about anything a
//! rule settles. This file holds the pieces around the call: the redacted and trimmed input ([`build_input`]),
//! the reading of the reply ([`parse_ruling`], broken output means "wait"), the cache of decisions and the
//! hourly spend cap ([`Advisor`]), and the [`Judge`] that reaches a provider through the same client the
//! agent loop uses. The calls run on a worker thread, so the supervisor's lock is never held while the
//! network answers.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use futures::StreamExt;
use nexo_core::config::ProviderConfig;
use nexo_core::message::Message;
use nexo_core::pricing::Pricing;
use nexo_core::provider::ModelRequest;
use nexo_core::stream::{Collector, StreamEvent};

use super::policy::ModelPolicy;
use super::{input_hash, AgentTrack, Secs};
use crate::client::ModelClient;

/// Longest text a ruling may carry.
pub const MAX_TEXT: usize = 400;
/// How many decisions the cache keeps.
const CACHE_MAX: usize = 256;
const HOUR: Secs = 3600;
/// How long one call may take before it counts as failed.
const CALL_TIMEOUT: Duration = Duration::from_secs(45);
/// A reply is short: two sentences and a word.
const MAX_REPLY_TOKENS: u32 = 200;
/// Characters per token the input cap assumes.
const CHARS_PER_TOKEN: usize = 4;

/// What the model is asked about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Case {
    /// The session ended its turn with a question nobody answered.
    Question,
    /// Errors keep coming and they are not the same one.
    Loop,
}

/// What the model decided.
#[derive(Clone, Debug, PartialEq)]
pub enum Ruling {
    Nudge(String),
    Alert(String),
    Wait,
}

const SYSTEM: &str = "You watch a long-running work session on behalf of its owner and decide how to keep it \
moving. You get a short, redacted summary. Reply with one JSON object and nothing else: \
{\"action\":\"nudge\"|\"alert\"|\"wait\",\"text\":\"...\"}. \"nudge\": text is what to tell the session, at most \
two short sentences, in the language the session uses. \"alert\": the owner must look; text says why in one \
line. \"wait\": do nothing. Never approve anything risky, never ask for secrets, never repeat the input. When \
unsure, answer wait.";

fn case_line(case: Case) -> &'static str {
    match case {
        Case::Question => {
            "Situation: the session ended its turn with a question to the owner and nobody answered. \
Answer it only if the answer is obvious and low-risk (continue with the plan); otherwise alert or wait."
        }
        Case::Loop => {
            "Situation: the session keeps hitting errors from different calls. Tell it in two sentences how to \
change approach, or alert if it needs the owner."
        }
    }
}

/// The text sent to the model for one session: redacted, and cut to `max_tokens`.
pub fn build_input(track: &AgentTrack, case: Case, max_tokens: u32) -> String {
    let mut s = String::new();
    s.push_str(case_line(case));
    s.push('\n');
    let name = track
        .name
        .clone()
        .or_else(|| {
            track.cwd.as_deref().and_then(|c| std::path::Path::new(c).file_name()).map(|f| f.to_string_lossy().into())
        })
        .unwrap_or_default();
    if !name.is_empty() {
        s.push_str(&format!("Session: {name}\n"));
    }
    if let Some((_, tail)) = &track.turn_end {
        s.push_str(&format!("Final text of the last turn:\n{tail}\n"));
    }
    if let Some(step) = &track.last_step {
        s.push_str(&format!("Last step: {step}\n"));
    }
    let marks: Vec<String> = track
        .results
        .iter()
        .rev()
        .take(8)
        .rev()
        .map(|r| format!("{} {}", r.name, if r.is_error { "failed" } else { "ok" }))
        .collect();
    if !marks.is_empty() {
        s.push_str(&format!("Latest tool results: {}\n", marks.join(", ")));
    }
    let clean = nexo_core::redact::redact(&s).into_owned();
    let cap = (max_tokens as usize).max(50) * CHARS_PER_TOKEN;
    clean.chars().take(cap).collect()
}

/// Read the model's reply. Anything that does not parse, or names an unknown action, is `Wait`.
pub fn parse_ruling(reply: &str) -> Ruling {
    let (Some(a), Some(b)) = (reply.find('{'), reply.rfind('}')) else { return Ruling::Wait };
    if b < a {
        return Ruling::Wait;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&reply[a..=b]) else { return Ruling::Wait };
    let text = v.get("text").and_then(|t| t.as_str()).unwrap_or("");
    let clean = nexo_core::redact::redact(text.trim()).into_owned();
    let text: String = clean.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_TEXT).collect();
    match v.get("action").and_then(|t| t.as_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("nudge") if !text.is_empty() => Ruling::Nudge(text),
        Some("alert") if !text.is_empty() => Ruling::Alert(text),
        _ => Ruling::Wait,
    }
}

/// What a call returned and what it cost.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Judged {
    pub text: String,
    pub cost_usd: f64,
}

/// One blocking question to a model.
pub trait Judge: Send + Sync {
    fn judge(&self, system: &str, input: &str) -> Result<Judged, String>;
    /// The model's name, for the log.
    fn model(&self) -> String;
}

/// A [`Judge`] over the agent loop's own model client.
pub struct ClientJudge {
    client: Arc<dyn ModelClient>,
    cfg: ProviderConfig,
    model: String,
    pricing: Option<Pricing>,
    rt: tokio::runtime::Handle,
}

impl ClientJudge {
    pub fn new(
        client: Arc<dyn ModelClient>,
        cfg: ProviderConfig,
        model: impl Into<String>,
        rt: tokio::runtime::Handle,
    ) -> ClientJudge {
        let model = model.into();
        let pricing = cfg.pricing_for(&model);
        ClientJudge { client, cfg, model, pricing, rt }
    }
}

impl Judge for ClientJudge {
    fn judge(&self, system: &str, input: &str) -> Result<Judged, String> {
        let (client, cfg, model, pricing) = (self.client.clone(), self.cfg.clone(), self.model.clone(), self.pricing);
        let (system, input) = (system.to_string(), input.to_string());
        let (tx, rx) = mpsc::channel();
        self.rt.spawn(async move {
            let mut req = ModelRequest::new(model, system, vec![Message::user(input)]);
            req.max_tokens = MAX_REPLY_TOKENS;
            req.cache = false;
            let out = async {
                let mut stream = client.stream(&cfg, &req, 0).await.map_err(|e| e.message)?;
                let mut c = Collector::new();
                while let Some(ev) = stream.next().await {
                    if let StreamEvent::Error(e) = &ev {
                        return Err(e.to_string());
                    }
                    c.push(ev);
                }
                let cost = pricing.map_or(0.0, |p| p.cost(&c.usage));
                Ok(Judged { text: c.message().text(), cost_usd: cost })
            }
            .await;
            let _ = tx.send(out);
        });
        rx.recv_timeout(CALL_TIMEOUT).unwrap_or_else(|_| Err("the model took too long".into()))
    }

    fn model(&self) -> String {
        self.model.clone()
    }
}

/// A judge for the model the policy names, found among the configured providers.
pub fn judge_for(
    policy: &ModelPolicy,
    providers: &[(String, ProviderConfig)],
    client: Arc<dyn ModelClient>,
    rt: tokio::runtime::Handle,
) -> Result<Arc<dyn Judge>, String> {
    if policy.provider.is_empty() || policy.name.is_empty() {
        return Err("[model] needs a provider and a name".into());
    }
    let Some((_, cfg)) = providers.iter().find(|(n, _)| *n == policy.provider) else {
        return Err(format!("[model] provider \"{}\" is not configured", policy.provider));
    };
    Ok(Arc::new(ClientJudge::new(client, cfg.clone(), policy.name.clone(), rt)))
}

/// The usual client, for the providers of the config.
pub fn default_client() -> Arc<dyn ModelClient> {
    Arc::new(nexo_providers::Client::new(nexo_providers::registry()))
}

struct Job {
    key: String,
    stamp: u64,
    system: &'static str,
    input: String,
}

/// A finished call.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub key: String,
    pub stamp: u64,
    pub ruling: Ruling,
    pub cost_usd: f64,
    pub model: String,
    pub error: Option<String>,
}

/// What asking got.
#[derive(Clone, Debug, PartialEq)]
pub enum Ask {
    /// This exact state was decided before.
    Cached(Ruling),
    /// The call is on its way (or was sent just now).
    Pending,
    /// The hourly cap stops it; only the rules act.
    Refused(&'static str),
}

/// Decisions cache plus the hourly budget, in front of a [`Judge`].
pub struct Advisor {
    policy: ModelPolicy,
    model: String,
    tx: Option<mpsc::Sender<Job>>,
    rx: mpsc::Receiver<Reply>,
    cache: HashMap<(String, u64), Ruling>,
    order: VecDeque<(String, u64)>,
    inflight: HashSet<(String, u64)>,
    calls: VecDeque<Secs>,
    spend: VecDeque<(Secs, f64)>,
}

impl Advisor {
    pub fn new(judge: Arc<dyn Judge>, policy: ModelPolicy) -> Advisor {
        let (tx, jobs) = mpsc::channel::<Job>();
        let (done, rx) = mpsc::channel::<Reply>();
        let model = judge.model();
        let name = model.clone();
        std::thread::spawn(move || {
            while let Ok(job) = jobs.recv() {
                let r = judge.judge(job.system, &job.input);
                let reply = match r {
                    Ok(j) => Reply {
                        key: job.key,
                        stamp: job.stamp,
                        ruling: parse_ruling(&j.text),
                        cost_usd: j.cost_usd,
                        model: name.clone(),
                        error: None,
                    },
                    Err(e) => Reply {
                        key: job.key,
                        stamp: job.stamp,
                        ruling: Ruling::Wait,
                        cost_usd: 0.0,
                        model: name.clone(),
                        error: Some(e),
                    },
                };
                if done.send(reply).is_err() {
                    break;
                }
            }
        });
        Advisor {
            policy,
            model,
            tx: Some(tx),
            rx,
            cache: HashMap::new(),
            order: VecDeque::new(),
            inflight: HashSet::new(),
            calls: VecDeque::new(),
            spend: VecDeque::new(),
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn policy(&self) -> &ModelPolicy {
        &self.policy
    }

    fn trim(&mut self, now: Secs) {
        while self.calls.front().is_some_and(|t| now - *t >= HOUR) {
            self.calls.pop_front();
        }
        while self.spend.front().is_some_and(|(t, _)| now - *t >= HOUR) {
            self.spend.pop_front();
        }
    }

    /// Dollars spent in the last hour.
    pub fn spent(&mut self, now: Secs) -> f64 {
        self.trim(now);
        self.spend.iter().map(|(_, c)| c).sum()
    }

    /// Calls made in the last hour.
    pub fn calls(&mut self, now: Secs) -> usize {
        self.trim(now);
        self.calls.len()
    }

    /// Ask about one session in one state. The same `(key, stamp)` is never asked twice; over the cap, nothing
    /// is sent.
    pub fn ask(&mut self, key: &str, stamp: u64, case: Case, track: &AgentTrack, now: Secs) -> Ask {
        let id = (key.to_string(), stamp);
        if let Some(r) = self.cache.get(&id) {
            return Ask::Cached(r.clone());
        }
        if self.inflight.contains(&id) {
            return Ask::Pending;
        }
        if self.calls(now) >= self.policy.max_calls_per_hour as usize {
            return Ask::Refused("model call limit for this hour reached");
        }
        if self.spent(now) >= self.policy.max_usd_per_hour {
            return Ask::Refused("model spending limit for this hour reached");
        }
        let input = build_input(track, case, self.policy.max_input_tokens);
        let Some(tx) = &self.tx else { return Ask::Refused("model unavailable") };
        if tx.send(Job { key: id.0.clone(), stamp, system: SYSTEM, input }).is_err() {
            return Ask::Refused("model unavailable");
        }
        self.calls.push_back(now);
        self.inflight.insert(id);
        Ask::Pending
    }

    /// Collect the calls that finished: cache them and count what they cost.
    pub fn poll(&mut self, now: Secs) -> Vec<Reply> {
        let mut out = Vec::new();
        while let Ok(r) = self.rx.try_recv() {
            let id = (r.key.clone(), r.stamp);
            self.inflight.remove(&id);
            if r.cost_usd > 0.0 {
                self.spend.push_back((now, r.cost_usd));
            }
            if self.cache.insert(id.clone(), r.ruling.clone()).is_none() {
                self.order.push_back(id);
            }
            while self.order.len() > CACHE_MAX {
                if let Some(old) = self.order.pop_front() {
                    self.cache.remove(&old);
                }
            }
            out.push(r);
        }
        out
    }
}

impl Drop for Advisor {
    fn drop(&mut self) {
        self.tx = None;
    }
}

/// The cache key of a state: what the session said or did last, so a changed state is a new question.
pub fn stamp(track: &AgentTrack, case: Case) -> u64 {
    let basis = match case {
        Case::Question => track.turn_end.as_ref().map_or(String::new(), |(at, tail)| format!("q{at}{tail}")),
        Case::Loop => {
            let marks: Vec<String> =
                track.results.iter().map(|r| format!("{}{}{}", r.name, r.input_hash, r.is_error)).collect();
            format!("l{}", marks.join(","))
        }
    };
    input_hash(&basis)
}

/// Whether the failing calls of a loop are different ones: only then is a model worth asking.
pub fn ambiguous_loop(track: &AgentTrack) -> bool {
    let mut seen: Vec<(&str, u64)> = Vec::new();
    for r in track.results.iter().filter(|r| r.is_error) {
        let k = (r.name.as_str(), r.input_hash);
        if !seen.contains(&k) {
            seen.push(k);
        }
    }
    seen.len() > 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervise::ResultMark;
    use std::sync::Mutex;

    struct Fake {
        replies: Mutex<VecDeque<Result<Judged, String>>>,
        seen: Mutex<Vec<String>>,
    }

    impl Fake {
        fn new(r: Vec<Result<Judged, String>>) -> Arc<Fake> {
            Arc::new(Fake { replies: Mutex::new(r.into()), seen: Mutex::new(Vec::new()) })
        }
    }

    impl Judge for Fake {
        fn judge(&self, _system: &str, input: &str) -> Result<Judged, String> {
            self.seen.lock().unwrap().push(input.to_string());
            self.replies.lock().unwrap().pop_front().unwrap_or_else(|| Ok(Judged::default()))
        }
        fn model(&self) -> String {
            "fake".into()
        }
    }

    fn ok(text: &str, cost: f64) -> Result<Judged, String> {
        Ok(Judged { text: text.into(), cost_usd: cost })
    }

    fn asking() -> AgentTrack {
        let mut t = AgentTrack::new("k");
        t.name = Some("build".into());
        t.turn_end = Some((100, "Shall I go on with the migration?".into()));
        t
    }

    fn wait_for(a: &mut Advisor, now: Secs, n: usize) -> Vec<Reply> {
        let mut got = Vec::new();
        for _ in 0..200 {
            got.extend(a.poll(now));
            if got.len() >= n {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        got
    }

    #[test]
    fn broken_output_means_wait() {
        for bad in ["", "yes do it", "{", "{\"action\":\"explode\",\"text\":\"x\"}", "{\"action\":\"nudge\"}", "[1,2]"]
        {
            assert_eq!(parse_ruling(bad), Ruling::Wait, "{bad}");
        }
        assert_eq!(
            parse_ruling("Sure:\n```json\n{\"action\":\"Nudge\",\"text\":\"  Yes,\\n go on. \"}\n```"),
            Ruling::Nudge("Yes, go on.".into())
        );
        assert_eq!(parse_ruling("{\"action\":\"alert\",\"text\":\"needs you\"}"), Ruling::Alert("needs you".into()));
    }

    #[test]
    fn a_reply_text_is_redacted_and_cut() {
        let long = "x".repeat(900);
        let key = "a1B2c3D4".repeat(5);
        let r = parse_ruling(&format!("{{\"action\":\"nudge\",\"text\":\"use sk-{key} {long}\"}}"));
        let Ruling::Nudge(t) = r else { panic!() };
        assert!(!t.contains("a1B2c3D4"), "{t}");
        assert!(t.chars().count() <= MAX_TEXT);
    }

    #[test]
    fn the_input_never_holds_a_seeded_secret_and_stays_within_the_cap() {
        let mut t = asking();
        let secret = &format!("sk-{}", "a1B2c3D4".repeat(5));
        t.turn_end =
            Some((1, format!("token={secret} and Bearer abcdefghijklmnop1234567890 then {}", "z".repeat(5000))));
        t.last_step = Some(format!("Bash  curl -H 'Authorization: Bearer {secret}' x"));
        t.results.push_back(ResultMark { name: "Bash".into(), input_hash: 1, is_error: true });
        let s = build_input(&t, Case::Question, 100);
        assert!(!s.contains(secret) && !s.contains("a1B2c3D4"), "{s}");
        assert!(!s.contains("abcdefghijklmnop1234567890"), "{s}");
        assert!(s.chars().count() <= 100 * CHARS_PER_TOKEN);
        let s = build_input(&t, Case::Loop, 2000);
        assert!(s.contains("Bash failed") && s.contains("Session: build"));
    }

    #[test]
    fn the_same_state_is_asked_once_and_a_new_one_again() {
        let fake = Fake::new(vec![ok("{\"action\":\"nudge\",\"text\":\"Yes, continue.\"}", 0.001)]);
        let mut a = Advisor::new(fake.clone(), ModelPolicy::default());
        let t = asking();
        let st = stamp(&t, Case::Question);
        assert_eq!(a.ask("k", st, Case::Question, &t, 10), Ask::Pending);
        assert_eq!(a.ask("k", st, Case::Question, &t, 11), Ask::Pending, "in flight: not sent twice");
        let got = wait_for(&mut a, 12, 1);
        assert_eq!(got.len(), 1);
        assert_eq!(a.ask("k", st, Case::Question, &t, 13), Ask::Cached(Ruling::Nudge("Yes, continue.".into())));
        assert_eq!(fake.seen.lock().unwrap().len(), 1, "one call");
        let mut t2 = asking();
        t2.turn_end = Some((500, "Should I also drop the old table?".into()));
        assert_eq!(a.ask("k", stamp(&t2, Case::Question), Case::Question, &t2, 20), Ask::Pending);
        wait_for(&mut a, 21, 1);
        assert_eq!(fake.seen.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_failed_call_is_cached_as_wait() {
        let fake = Fake::new(vec![Err("boom".into())]);
        let mut a = Advisor::new(fake, ModelPolicy::default());
        let t = asking();
        let st = stamp(&t, Case::Question);
        a.ask("k", st, Case::Question, &t, 1);
        let got = wait_for(&mut a, 2, 1);
        assert_eq!(got[0].error.as_deref(), Some("boom"));
        assert_eq!(a.ask("k", st, Case::Question, &t, 3), Ask::Cached(Ruling::Wait));
    }

    #[test]
    fn the_hourly_cap_on_dollars_stops_calls_and_lifts_after_the_hour() {
        let fake = Fake::new(vec![ok("{\"action\":\"wait\"}", 0.30)]);
        let pol = ModelPolicy { max_usd_per_hour: 0.20, ..Default::default() };
        let mut a = Advisor::new(fake.clone(), pol);
        let t = asking();
        a.ask("k", 1, Case::Question, &t, 100);
        wait_for(&mut a, 101, 1);
        assert!((a.spent(101) - 0.30).abs() < 1e-9);
        assert!(matches!(a.ask("k", 2, Case::Question, &t, 102), Ask::Refused(_)));
        assert_eq!(fake.seen.lock().unwrap().len(), 1, "no second call");
        assert_eq!(a.ask("k", 3, Case::Question, &t, 100 + HOUR + 1), Ask::Pending, "the hour slid by");
    }

    #[test]
    fn the_hourly_cap_on_calls_stops_calls() {
        let fake = Fake::new(vec![]);
        let pol = ModelPolicy { max_calls_per_hour: 2, ..Default::default() };
        let mut a = Advisor::new(fake, pol);
        let t = asking();
        assert_eq!(a.ask("a", 1, Case::Question, &t, 10), Ask::Pending);
        assert_eq!(a.ask("b", 1, Case::Question, &t, 11), Ask::Pending);
        assert!(matches!(a.ask("c", 1, Case::Question, &t, 12), Ask::Refused(_)));
    }

    #[test]
    fn only_different_errors_make_a_loop_ambiguous() {
        let mut t = AgentTrack::new("k");
        for h in [1, 1, 1] {
            t.results.push_back(ResultMark { name: "Bash".into(), input_hash: h, is_error: true });
        }
        assert!(!ambiguous_loop(&t));
        t.results.push_back(ResultMark { name: "Bash".into(), input_hash: 2, is_error: true });
        assert!(ambiguous_loop(&t));
        assert_ne!(stamp(&t, Case::Loop), {
            t.results.pop_back();
            stamp(&t, Case::Loop)
        });
    }

    #[test]
    fn the_judge_for_a_policy_needs_a_configured_provider() {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let pol = ModelPolicy { provider: "nope".into(), name: "m".into(), ..Default::default() };
        let err = judge_for(&pol, &[], default_client(), rt.handle().clone()).err().unwrap();
        assert!(err.contains("nope"));
        let err = judge_for(&ModelPolicy::default(), &[], default_client(), rt.handle().clone()).err().unwrap();
        assert!(err.contains("provider"));
    }

    #[test]
    fn the_client_judge_talks_to_the_mock_provider() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("s.json");
        std::fs::write(
            &script,
            r#"[[{"text":"{\"action\":\"nudge\",\"text\":\"Yes, go on.\"}"},{"usage":{"input":1000000,"output":0,"cache_read":0,"cache_write":0,"reasoning":0}}]]"#,
        )
        .unwrap();
        let cfg: ProviderConfig = toml::from_str(&format!(
            "protocol = 'mock'\nmodels = ['m']\nextra = {{ script = '{}' }}\n[pricing.m]\ninput = 0.5\noutput = 1.0\n",
            script.display().to_string().replace('\\', "/")
        ))
        .unwrap();
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let pol = ModelPolicy { provider: "mock".into(), name: "m".into(), ..Default::default() };
        let judge = judge_for(&pol, &[("mock".into(), cfg)], default_client(), rt.handle().clone()).unwrap();
        let mut a = Advisor::new(judge, pol);
        let t = asking();
        let st = stamp(&t, Case::Question);
        assert_eq!(a.ask("k", st, Case::Question, &t, 5), Ask::Pending);
        let got = wait_for(&mut a, 6, 1);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].error, None);
        assert_eq!(got[0].ruling, Ruling::Nudge("Yes, go on.".into()));
        assert!((got[0].cost_usd - 0.5).abs() < 1e-9, "{}", got[0].cost_usd);
    }
}
