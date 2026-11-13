//! Page events kept for later reading: console messages and network requests.

use crate::{Error, Result};
use chromiumoxide::cdp::browser_protocol::log::{EnableParams as LogEnable, EventEntryAdded};
use chromiumoxide::cdp::browser_protocol::network::{
    EnableParams as NetworkEnable, EventRequestWillBeSent, EventResponseReceived,
};
use chromiumoxide::cdp::js_protocol::runtime::{
    EnableParams as RuntimeEnable, EventConsoleApiCalled, EventExceptionThrown, RemoteObject,
};
use chromiumoxide::cdp::IntoEventKind;
use chromiumoxide::page::Page;
use futures::StreamExt;
use regex::Regex;
use serde::Serialize;
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

/// One network request of the page. The status is `None` until a response arrives.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NetEntry {
    pub method: String,
    pub url: String,
    pub status: Option<u16>,
    /// Milliseconds since the Unix epoch.
    pub ts: f64,
    #[serde(skip)]
    id: String,
}

/// The console and network buffers, shared by the watchers of every tab.
#[derive(Clone, Default)]
pub(crate) struct Buffers {
    console: Arc<Mutex<Vec<ConsoleEntry>>>,
    net: Arc<Mutex<Vec<NetEntry>>>,
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
        let net = Arc::clone(&self.net);
        tasks.push(
            listen::<EventRequestWillBeSent, _>(page, move |e| {
                let id = e.request_id.inner().clone();
                if let Some(redirect) = &e.redirect_response {
                    let mut net = lock(&net);
                    if let Some(prev) = net.iter_mut().rev().find(|n| n.id == id && n.status.is_none()) {
                        prev.status = u16::try_from(redirect.status).ok();
                    }
                }
                let entry = NetEntry {
                    method: e.request.method.clone(),
                    url: e.request.url.clone(),
                    status: None,
                    ts: *e.wall_time.inner() * 1_000.0,
                    id,
                };
                push(&net, entry);
            })
            .await?,
        );
        let net = Arc::clone(&self.net);
        tasks.push(
            listen::<EventResponseReceived, _>(page, move |e| {
                let id = e.request_id.inner();
                if let Some(hit) = lock(&net).iter_mut().rev().find(|n| &n.id == id && n.status.is_none()) {
                    hit.status = u16::try_from(e.response.status).ok();
                }
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
}
