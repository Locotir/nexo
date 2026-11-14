//! Page events kept for later reading: console messages and network requests.

use crate::{redact, Error, Result};
use chromiumoxide::cdp::browser_protocol::log::{EnableParams as LogEnable, EventEntryAdded};
use chromiumoxide::cdp::browser_protocol::network::{
    EnableParams as NetworkEnable, EventLoadingFailed, EventLoadingFinished, EventRequestWillBeSent,
    EventResponseReceived,
};
use chromiumoxide::cdp::js_protocol::runtime::{
    EnableParams as RuntimeEnable, EventConsoleApiCalled, EventExceptionThrown, RemoteObject,
};
use chromiumoxide::cdp::IntoEventKind;
use chromiumoxide::page::Page;
use futures::StreamExt;
use regex::Regex;
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::task::JoinHandle;

mod relay;

/// Entries kept per buffer; the oldest are dropped first.
const BUFFER_CAP: usize = 1_000;

/// One console message of the page: a `console.*` call, an uncaught exception or a browser log line.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConsoleEntry {
    /// `log`, `info`, `warning`, `error`, `debug`, ...
    pub level: String,
    pub text: String,
    /// Milliseconds since the Unix epoch.
    pub ts: f64,
}

/// One network request of the page. The status is `None` until a response arrives. Credentials never reach it:
/// secret headers and URL parameters are recorded as [`crate::REDACTED`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NetEntry {
    /// Number of the request since launch (1, 2, ...), the handle for `network_detail`.
    pub seq: u64,
    pub method: String,
    pub url: String,
    pub status: Option<u16>,
    /// Milliseconds since the Unix epoch.
    pub ts: f64,
    pub status_text: String,
    pub mime: String,
    pub request_headers: Vec<(String, String)>,
    pub response_headers: Vec<(String, String)>,
    /// Milliseconds until the first byte of the response, when the browser measured it.
    pub first_byte_ms: Option<f64>,
    /// Milliseconds from the request to the end of the response (or to its failure).
    pub duration_ms: Option<f64>,
    /// Why the request failed (blocked, refused, ...); `None` when it did not.
    pub error: Option<String>,
    #[serde(skip)]
    pub(crate) id: String,
    /// The tab it came from: the target id of a headless tab, the tab id of the person's own browser.
    #[serde(skip)]
    pub(crate) source: String,
    /// The browser's monotonic clock at the request, in seconds; `None` when it was not sent.
    #[serde(skip)]
    started: Option<f64>,
}

impl NetEntry {
    /// A new request with nothing answered yet; the URL is redacted here.
    pub(crate) fn new(seq: u64, method: String, url: &str, ts: f64, source: String, id: String) -> NetEntry {
        NetEntry {
            seq,
            method,
            url: redact::url(url),
            status: None,
            ts,
            status_text: String::new(),
            mime: String::new(),
            request_headers: Vec::new(),
            response_headers: Vec::new(),
            first_byte_ms: None,
            duration_ms: None,
            error: None,
            id,
            source,
            started: None,
        }
    }

    /// A finished `POST https://a.b/api` with headers and timing, for the tests of the tool that shows it.
    #[doc(hidden)]
    pub fn sample(seq: u64) -> NetEntry {
        NetEntry {
            status: Some(201),
            status_text: "Created".into(),
            mime: "application/json".into(),
            request_headers: vec![("Accept".into(), "*/*".into())],
            response_headers: vec![("Content-Type".into(), "application/json".into())],
            first_byte_ms: Some(80.2),
            duration_ms: Some(120.4),
            ..NetEntry::new(seq, "POST".into(), "https://a.b/api", 0.0, "sample".into(), "sample".into())
        }
    }

    /// Milliseconds since the request, at this monotonic time (seconds).
    fn since(&self, now: Option<f64>) -> Option<f64> {
        Some((now? - self.started?) * 1_000.0)
    }
}

/// The console and network buffers, shared by the watchers of every tab.
#[derive(Clone, Default)]
pub(crate) struct Buffers {
    console: Arc<Mutex<Vec<ConsoleEntry>>>,
    net: Arc<Mutex<Vec<NetEntry>>>,
    seq: Arc<AtomicU64>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn push<T>(buf: &Mutex<Vec<T>>, item: T) {
    let mut buf = lock(buf);
    if buf.len() >= BUFFER_CAP {
        buf.remove(0);
    }
    buf.push(item);
}

fn level_of(debug: &impl std::fmt::Debug) -> String {
    format!("{debug:?}").to_lowercase()
}

fn show(arg: &RemoteObject) -> String {
    match &arg.value {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => arg.description.clone().unwrap_or_default(),
    }
}

fn filter_regex(filter: Option<&str>) -> Result<Option<Regex>> {
    filter
        .map(|f| Regex::new(f).map_err(|e| Error::Unusable { target: f.to_string(), reason: e.to_string() }))
        .transpose()
}

async fn listen<T, F>(page: &Page, mut on: F) -> Result<JoinHandle<()>>
where
    T: IntoEventKind + Unpin + Send + Sync + 'static,
    F: FnMut(&T) + Send + 'static,
{
    let mut stream = page.event_listener::<T>().await?;
    Ok(tokio::spawn(async move {
        while let Some(event) = stream.next().await {
            on(&event);
        }
    }))
}

impl Buffers {
    /// Enables the Runtime, Log and Network domains of the page and starts filling the buffers from it.
    pub(crate) async fn watch(&self, page: &Page) -> Result<Vec<JoinHandle<()>>> {
        let mut tasks = Vec::new();
        let console = Arc::clone(&self.console);
        tasks.push(
            listen::<EventConsoleApiCalled, _>(page, move |e| {
                let text = e.args.iter().map(show).collect::<Vec<_>>().join(" ");
                push(&console, ConsoleEntry { level: level_of(&e.r#type), text, ts: *e.timestamp.inner() });
            })
            .await?,
        );
        let console = Arc::clone(&self.console);
        tasks.push(
            listen::<EventExceptionThrown, _>(page, move |e| {
                let d = &e.exception_details;
                let text = d.exception.as_ref().and_then(|x| x.description.clone()).unwrap_or_else(|| d.text.clone());
                push(&console, ConsoleEntry { level: "error".into(), text, ts: *e.timestamp.inner() });
            })
            .await?,
        );
        let console = Arc::clone(&self.console);
        tasks.push(
            listen::<EventEntryAdded, _>(page, move |e| {
                let entry = ConsoleEntry {
                    level: level_of(&e.entry.level),
                    text: e.entry.text.clone(),
                    ts: *e.entry.timestamp.inner(),
                };
                push(&console, entry);
            })
            .await?,
        );
        let (buffers, source) = (self.clone(), page.target_id().inner().clone());
        tasks.push(
            listen::<EventRequestWillBeSent, _>(page, move |e| {
                let id = e.request_id.inner().clone();
                let now = Some(*e.timestamp.inner());
                if let Some(r) = &e.redirect_response {
                    buffers.answered(
                        &id,
                        u16::try_from(r.status).ok(),
                        &r.status_text,
                        &r.mime_type,
                        r.headers.inner(),
                    );
                    buffers.ended(&id, now, None);
                }
                let mut entry = NetEntry::new(
                    buffers.next_seq(),
                    e.request.method.clone(),
                    &e.request.url,
                    *e.wall_time.inner() * 1_000.0,
                    source.clone(),
                    id,
                );
                entry.request_headers = redact::header_list(e.request.headers.inner());
                entry.started = now;
                push(&buffers.net, entry);
            })
            .await?,
        );
        let buffers = self.clone();
        tasks.push(
            listen::<EventResponseReceived, _>(page, move |e| {
                let r = &e.response;
                let id = e.request_id.inner();
                buffers.answered(id, u16::try_from(r.status).ok(), &r.status_text, &r.mime_type, r.headers.inner());
                if let Some(t) = &r.timing {
                    if let Some(hit) = lock(&buffers.net).iter_mut().rev().find(|n| &n.id == id) {
                        hit.first_byte_ms = Some(t.receive_headers_end);
                    }
                }
            })
            .await?,
        );
        let buffers = self.clone();
        tasks.push(
            listen::<EventLoadingFinished, _>(page, move |e| {
                buffers.ended(e.request_id.inner(), Some(*e.timestamp.inner()), None);
            })
            .await?,
        );
        let buffers = self.clone();
        tasks.push(
            listen::<EventLoadingFailed, _>(page, move |e| {
                buffers.ended(e.request_id.inner(), Some(*e.timestamp.inner()), Some(e.error_text.clone()));
            })
            .await?,
        );
        page.execute(RuntimeEnable::default()).await?;
        page.execute(LogEnable::default()).await?;
        page.execute(NetworkEnable::default()).await?;
        Ok(tasks)
    }

    /// The console messages so far whose text or level matches the regex; all of them without a filter.
    pub(crate) fn console(&self, filter: Option<&str>) -> Result<Vec<ConsoleEntry>> {
        let re = filter_regex(filter)?;
        let all = lock(&self.console).clone();
        Ok(all
            .into_iter()
            .filter(|c| re.as_ref().is_none_or(|r| r.is_match(&c.text) || r.is_match(&c.level)))
            .collect())
    }

    /// The requests so far whose URL or method matches the regex; all of them without a filter.
    pub(crate) fn network(&self, filter: Option<&str>) -> Result<Vec<NetEntry>> {
        let re = filter_regex(filter)?;
        let all = lock(&self.net).clone();
        Ok(all
            .into_iter()
            .filter(|n| re.as_ref().is_none_or(|r| r.is_match(&n.url) || r.is_match(&n.method)))
            .collect())
    }

    /// The request with this number (see [`NetEntry::seq`]), if it is still among the last ones kept.
    pub(crate) fn request(&self, seq: u64) -> Option<NetEntry> {
        lock(&self.net).iter().find(|n| n.seq == seq).cloned()
    }

    pub(crate) fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// The response of the latest unanswered request with this id; its headers are redacted here.
    pub(crate) fn answered(&self, id: &str, status: Option<u16>, text: &str, mime: &str, headers: &serde_json::Value) {
        if let Some(hit) = lock(&self.net).iter_mut().rev().find(|n| n.id == id && n.status.is_none()) {
            hit.status = status;
            hit.status_text = text.to_string();
            hit.mime = mime.to_string();
            hit.response_headers = redact::header_list(headers);
        }
    }

    /// The end of the latest request with this id: its duration (from the monotonic `now`) and why it failed.
    pub(crate) fn ended(&self, id: &str, now: Option<f64>, error: Option<String>) {
        if let Some(hit) = lock(&self.net).iter_mut().rev().find(|n| n.id == id && n.duration_ms.is_none()) {
            hit.duration_ms = hit.since(now);
            if error.is_some() {
                hit.error = error;
            }
        }
    }
}
