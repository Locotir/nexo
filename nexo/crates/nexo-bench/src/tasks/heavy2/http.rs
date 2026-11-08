//! HTTP jobs: a long access log and a local responder (`http-trace`), two raw API responses whose shape
//! changed (`http-api-diff`) and a HAR-like capture with one cookie sent where it should not go
//! (`http-har-leak`). The responder listens on `127.0.0.1` only.

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::{compile_rs, have, keyed, line};
use crate::check::{run, Outcome};
use crate::rng::Rng;
use crate::tasks::{files, Task};

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn task(
    id: &'static str,
    trap: &'static str,
    list: &[(&str, &str)],
    turns: Vec<(u32, String)>,
    check: fn(&Path) -> Outcome,
    solution: fn(&Path),
    setup: Option<fn(&Path) -> Result<(), String>>,
    ready: fn() -> Result<(), String>,
) -> Task {
    Task {
        id,
        trap,
        group: "heavy2",
        files: files(list),
        turns,
        check,
        solution,
        setup,
        sealed: false,
        mask: &[],
        ready,
    }
}

pub fn tasks() -> Vec<Task> {
    vec![trace_task(), diff_task(), har_task()]
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

// ---------------------------------------------------------------- http-trace

const RESPONDER_RS: &str = r#"use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

// usage: responder PORT ORDERS
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let orders: u32 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(17);
    let l = TcpListener::bind(format!("127.0.0.1:{}", a[1])).expect("bind");
    for s in l.incoming().flatten() {
        let _ = handle(s, orders);
    }
}

fn handle(mut s: TcpStream, orders: u32) -> std::io::Result<()> {
    let mut r = BufReader::new(s.try_clone()?);
    let mut first = String::new();
    r.read_line(&mut first)?;
    let (mut cookie, mut ver) = (String::new(), String::new());
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 || h.trim().is_empty() {
            break;
        }
        let low = h.to_ascii_lowercase();
        if low.starts_with("cookie:") {
            cookie = h[7..].trim().to_string();
        }
        if low.starts_with("x-api-version:") {
            ver = h[14..].trim().to_string();
        }
    }
    let path = first.split_whitespace().nth(1).unwrap_or("/").to_string();
    let (status, head, body) = if path == "/login" {
        ("302 Found", "Location: /session?next=/api/orders\r\n".to_string(), String::new())
    } else if path.starts_with("/session") {
        ("302 Found", "Set-Cookie: sid=abc123; Path=/\r\nLocation: /api/orders\r\n".to_string(), String::new())
    } else if path == "/api/orders" {
        if cookie.contains("sid=abc123") && ver == "2" {
            ("200 OK", "Content-Type: application/json\r\n".to_string(), format!("{{\"orders\":{orders}}}"))
        } else {
            ("401 Unauthorized", "Content-Type: application/json\r\n".to_string(), "{\"error\":\"unauthorized\"}".to_string())
        }
    } else {
        ("404 Not Found", String::new(), "not found".to_string())
    };
    write!(s, "HTTP/1.1 {status}\r\n{head}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())?;
    Ok(())
}
"#;

const CLIENT_SH: &str = r#"#!/bin/sh
# usage: sh client.sh PORT      prints "orders: N"
curl -s "http://127.0.0.1:$1/api/orders" | sed -n 's/.*"orders":\([0-9]*\).*/orders: \1/p'
"#;

const TRACE_LINE: usize = 431;

/// 600 lines of an access log with exactly one 401, at `TRACE_LINE`.
fn access_log() -> String {
    let mut r = Rng::new(0x10b);
    let mut out = String::new();
    for i in 1..=600usize {
        let (m, s) = (i / 60 % 60, i % 60);
        let stamp = format!("[08/Nov/2026:10:{m:02}:{s:02} +0000]");
        let (path, status, bytes, who) = if i == TRACE_LINE {
            ("/api/orders".to_string(), 401, 24, "curl/8.5.0 sid=- ver=-")
        } else {
            match r.below(10) {
                0 => ("/health".to_string(), 200, 2, "probe/1.0 sid=- ver=-"),
                1 | 2 => (
                    format!("/static/app.{}.js", r.below(9)),
                    if r.chance(0.5) { 304 } else { 200 },
                    r.int(0, 90_000),
                    "browser/9 sid=- ver=-",
                ),
                3 => (
                    format!("/api/items/{}", r.int(1, 400)),
                    if r.chance(0.1) { 404 } else { 200 },
                    r.int(100, 900),
                    "client.sh/2.0 sid=abc123 ver=2",
                ),
                4 => ("/login".to_string(), 302, 0, "client.sh/2.0 sid=- ver=2"),
                5 => ("/session?next=/api/orders".to_string(), 302, 0, "client.sh/2.0 sid=- ver=2"),
                6 => ("/api/orders".to_string(), 200, 14, "client.sh/2.0 sid=abc123 ver=2"),
                _ => (format!("/static/logo.{}.png", r.below(5)), 200, r.int(1000, 40_000), "browser/9 sid=- ver=-"),
            }
        };
        let method = if path.starts_with("/api/items") && r.chance(0.2) { "POST" } else { "GET" };
        out += &format!("127.0.0.1 - - {stamp} \"{method} {path} HTTP/1.1\" {status} {bytes} \"-\" \"{who}\"\n");
    }
    out
}

fn trace_task() -> Task {
    let rules = leak(
        "TRACE - rules of the task

GOAL
  client.sh asks the local order service for the number of orders. It gets nothing back. The
  service's access log is in access.log. Find the request that was refused and fix the client.

WHAT YOU HAVE
  access.log     600 lines of the service's access log (the last two fields of a line are the user
                 agent, the session cookie and the API version the request carried)
  responder.rs   the service (std only); `responder` is its build. Start it with
                 ./responder PORT ORDERS     (it listens on 127.0.0.1 only)
  client.sh      the client: sh client.sh PORT   must print `orders: N`
  Use curl against 127.0.0.1 only; there is no other network.

THE FINAL ANSWERS
  answer.txt, one line:   line: <the line number in access.log of the request that was refused (401)>
  client.sh, fixed so that `sh client.sh PORT` prints `orders: N` for whatever service answers on PORT.
  When you are sure, stop and reply in a few lines.
"
        .into(),
    );
    task(
        "http-trace",
        "a 600-line access log with one refusal, and a redirect-and-cookie handshake the client skips",
        &[
            ("RULES.md", rules),
            ("access.log", leak(access_log())),
            ("client.sh", CLIENT_SH),
            ("responder.rs", RESPONDER_RS),
        ],
        vec![(
            1,
            "client.sh prints nothing when it should print the number of orders. Read RULES.md first; find the refused \
             request in access.log, write its line number in answer.txt and fix client.sh."
                .into(),
        )],
        check_trace,
        solution_trace,
        Some(setup_trace),
        ready_trace,
    )
}

fn ready_trace() -> Result<(), String> {
    have(&["curl", "rustc", "sh", "sed"])
}

fn setup_trace(wd: &Path) -> Result<(), String> {
    let got = compile_rs(&wd.join("responder.rs"), &wd.join("responder"));
    if got.success() {
        Ok(())
    } else {
        Err(format!("responder did not compile: {}", got.tail(400)))
    }
}

/// The checker's own responder, built from the shipped source in a folder of its own.
fn build_responder(dir: &Path) -> Result<std::path::PathBuf, String> {
    let src = dir.join("r.rs");
    std::fs::write(&src, RESPONDER_RS).map_err(|e| e.to_string())?;
    let bin = dir.join("r");
    let got = compile_rs(&src, &bin);
    if got.success() {
        Ok(bin)
    } else {
        Err(format!("responder did not compile: {}", got.tail(300)))
    }
}

fn free_port() -> Option<u16> {
    TcpListener::bind("127.0.0.1:0").ok()?.local_addr().ok().map(|a| a.port())
}

/// What `sh client.sh` prints against a responder that reports `orders` orders.
fn client_says(client: &str, bin: &Path, orders: u32) -> Result<String, String> {
    let port = free_port().ok_or("no free port")?;
    let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
    std::fs::write(tmp.path().join("client.sh"), client).map_err(|e| e.to_string())?;
    let mut child = Command::new(bin)
        .args([port.to_string(), orders.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let start = Instant::now();
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        if start.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            return Err("the responder did not start".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let got = run(
        Command::new("sh").arg("client.sh").arg(port.to_string()).current_dir(tmp.path()),
        None,
        Duration::from_secs(30),
    );
    let _ = child.kill();
    let _ = child.wait();
    Ok(got.stdout)
}

fn check_trace(wd: &Path) -> Outcome {
    let a = keyed(wd, "answer.txt");
    let mut parts = vec![line(&a, "line", &TRACE_LINE.to_string())];
    let client = std::fs::read_to_string(wd.join("client.sh")).unwrap_or_default();
    let tmp = match tempfile::tempdir() {
        Ok(t) => t,
        Err(e) => return Outcome::fail(e.to_string()),
    };
    match build_responder(tmp.path()) {
        Err(e) => return Outcome::fail(e),
        Ok(bin) => {
            for orders in [17u32, 23] {
                let said = client_says(&client, &bin, orders).unwrap_or_else(|e| e);
                let want = format!("orders: {orders}");
                parts.push((
                    said.lines().any(|l| l.trim() == want),
                    format!("client.sh against {orders} orders printed {said:?}"),
                ));
            }
        }
    }
    Outcome::share(parts)
}

fn solution_trace(wd: &Path) {
    let _ = std::fs::write(wd.join("answer.txt"), format!("line: {TRACE_LINE}\n"));
    let fixed = "#!/bin/sh\n# usage: sh client.sh PORT      prints \"orders: N\"\nj=$(mktemp)\ncurl -s -L -c \"$j\" -b \"$j\" -H 'X-Api-Version: 2' \"http://127.0.0.1:$1/login\" | sed -n 's/.*\"orders\":\\([0-9]*\\).*/orders: \\1/p'\nrm -f \"$j\"\n";
    let _ = std::fs::write(wd.join("client.sh"), fixed);
}

// ---------------------------------------------------------------- http-api-diff

const CLIENT_RS: &str = r#"use std::io::Read;

// Reads a raw HTTP response on stdin and prints the order total in cents.
fn numbers_after(body: &str, key: &str) -> Vec<i64> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find(key) {
        rest = &rest[i + key.len()..];
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = digits.parse() {
            out.push(n);
        }
    }
    out
}

fn main() {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw).unwrap();
    let body = raw.split_once("\r\n\r\n").map(|x| x.1).unwrap_or(&raw);
    let qty = numbers_after(body, "\"qty\":");
    let price = numbers_after(body, "\"price\":");
    let total: i64 = qty.iter().zip(&price).map(|(q, p)| q * p).sum();
    println!("total: {total}");
}
"#;

/// A raw response of API `version` (1 or 2) with 60 lines, and the order total in cents.
fn payload(version: u8, seed: u64) -> (String, i64) {
    let mut r = Rng::new(seed);
    let (mut lines, mut total) = (Vec::new(), 0i64);
    for i in 0..60 {
        let (sku, q, p) = (format!("SKU-{:03}", r.int(1, 400)), r.int(1, 9), r.int(150, 9900));
        if version == 1 {
            total += q * p;
            lines.push(format!("{{\"sku\":\"{sku}\",\"qty\":{q},\"price\":{p}}}"));
        } else {
            let d = if i % 5 == 0 { r.int(10, 140) } else { 0 };
            total += q * p - d;
            lines.push(format!(
                "{{\"sku\":\"{sku}\",\"quantity\":{q},\"unit\":{{\"cents\":{p},\"cur\":\"EUR\"}},\"discount_cents\":{d}}}"
            ));
        }
    }
    let body = if version == 1 {
        format!("{{\"items\":[{}],\"count\":60}}", lines.join(","))
    } else {
        format!("{{\"data\":{{\"lines\":[{}],\"next\":null}}}}", lines.join(","))
    };
    let raw = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-Api-Version: {version}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    (raw, total)
}

fn diff_task() -> Task {
    let rules = leak(
        "API-DIFF - rules of the task

GOAL
  The order service moved from API v1 to API v2. client.rs reads a raw HTTP response on stdin and
  prints the order total in cents; it was written for v1. Make it right for both.

WHAT YOU HAVE
  v1.http, v2.http   one raw response of each version (headers, a blank line, one JSON body)
  client.rs          the client:  rustc client.rs && ./client < v1.http     prints `total: N`
  There is no network and no service: the two files are all the traffic.

WHAT THE API DOCS SAY ABOUT v2
  The lines moved under data.lines; a line has `quantity` and `unit.cents`, and an optional
  `discount_cents` taken off the line (a line is worth quantity * unit.cents - discount_cents).
  The order total is the sum of the lines. v1 is unchanged and must keep working.

THE FINAL ANSWER
  client.rs, fixed so that it prints the right total for any v1 or v2 response of this shape.
  Do not hard-code the totals of the two files. When you are sure, stop and reply in a few lines.
"
        .into(),
    );
    let (v1, _) = payload(1, 11);
    let (v2, _) = payload(2, 12);
    task(
        "http-api-diff",
        "two near-identical dumps whose field names, nesting and a new discount field differ; the diff is in the \
         details of a long single-line body",
        &[("RULES.md", rules), ("v1.http", leak(v1)), ("v2.http", leak(v2)), ("client.rs", CLIENT_RS)],
        vec![(
            1,
            "client.rs gives the right total for v1.http and a wrong one for v2.http. Read RULES.md first and fix \
             client.rs so both work."
                .into(),
        )],
        check_diff,
        solution_diff,
        None,
        || have(&["rustc"]),
    )
}

fn check_diff(wd: &Path) -> Outcome {
    let src = wd.join("client.rs");
    let tmp = match tempfile::tempdir() {
        Ok(t) => t,
        Err(e) => return Outcome::fail(e.to_string()),
    };
    let bin = tmp.path().join("c");
    let built = compile_rs(&src, &bin);
    if !built.success() {
        return Outcome::fail(format!("client.rs does not compile: {}", built.tail(300)));
    }
    let mut parts = Vec::new();
    for (version, seed) in [(1u8, 11u64), (2, 12), (1, 21), (2, 22), (2, 23)] {
        let (raw, total) = payload(version, seed);
        let got = run(&mut Command::new(&bin), Some(&raw), Duration::from_secs(20));
        parts.push((
            got.stdout.trim() == format!("total: {total}"),
            format!("v{version} seed {seed}: printed {:?}, expected total {total}", got.stdout.trim()),
        ));
    }
    Outcome::share(parts)
}

fn solution_diff(wd: &Path) {
    let fixed = CLIENT_RS.replace(
        "    let qty = numbers_after(body, \"\\\"qty\\\":\");\n    let price = numbers_after(body, \"\\\"price\\\":\");\n    let total: i64 = qty.iter().zip(&price).map(|(q, p)| q * p).sum();\n",
        "    let total: i64 = if body.contains(\"\\\"quantity\\\":\") {\n        let q = numbers_after(body, \"\\\"quantity\\\":\");\n        let p = numbers_after(body, \"\\\"cents\\\":\");\n        let d = numbers_after(body, \"\\\"discount_cents\\\":\");\n        q.iter().zip(&p).map(|(q, p)| q * p).sum::<i64>() - d.iter().sum::<i64>()\n    } else {\n        let q = numbers_after(body, \"\\\"qty\\\":\");\n        let p = numbers_after(body, \"\\\"price\\\":\");\n        q.iter().zip(&p).map(|(q, p)| q * p).sum()\n    };\n",
    );
    let _ = std::fs::write(wd.join("client.rs"), fixed);
}

// ---------------------------------------------------------------- http-har-leak

const HAR_ENTRY: usize = 337;

/// `(capture text, the session cookie that leaks)`: 400 entries, one cookie sent to a third party.
fn har() -> (String, String) {
    let mut r = Rng::new(0x4a2);
    let hex = |r: &mut Rng, n: usize| -> String {
        (0..n).map(|_| char::from_digit(r.below(16) as u32, 16).unwrap()).collect()
    };
    let (sid1, sid2) = (hex(&mut r, 32), hex(&mut r, 32));
    let mut out = String::new();
    for n in 1..=400usize {
        let sid = if n < 211 { &sid1 } else { &sid2 };
        let (host, path, status) = if n == HAR_ENTRY {
            ("ads.tracker.example.test".to_string(), format!("/px.gif?ref=%2Fcart&u={sid2}"), "200 OK")
        } else if n == 150 {
            ("shop.example.test".to_string(), format!("/redirect?sid={sid1}&to=%2Fcart"), "302 Found")
        } else if n == 90 || n == 250 {
            ("ads.tracker.example.test".to_string(), format!("/px.gif?ref=%2Fhome&u={}", hex(&mut r, 32)), "200 OK")
        } else {
            match r.below(6) {
                0 => ("cdn.static.example.test".to_string(), format!("/v{}/app.js", r.below(9)), "200 OK"),
                1 => (
                    "metrics.example.test".to_string(),
                    format!("/ping?t={}", r.int(1_000_000, 9_999_999)),
                    "204 No Content",
                ),
                2 => ("shop.example.test".to_string(), format!("/api/items/{}", r.int(1, 400)), "200 OK"),
                3 => ("shop.example.test".to_string(), "/cart".to_string(), "200 OK"),
                4 => ("shop.example.test".to_string(), format!("/search?q=item{}", r.int(1, 90)), "200 OK"),
                _ => ("cdn.static.example.test".to_string(), format!("/img/{}.png", hex(&mut r, 6)), "200 OK"),
            }
        };
        out += &format!("entry {n}\n> GET https://{host}{path} HTTP/1.1\n> Host: {host}\n");
        if host == "shop.example.test" && n > 2 {
            out += &format!("> Cookie: sid={sid}\n");
        }
        out += &format!("< {status}\n");
        if n == 3 {
            out += &format!("< Set-Cookie: sid={sid1}; HttpOnly; Secure\n");
        }
        if n == 211 {
            out += &format!("< Set-Cookie: sid={sid2}; HttpOnly; Secure\n");
        }
        out.push('\n');
    }
    (out, sid2)
}

fn har_task() -> Task {
    let rules = leak(
        "HAR-LEAK - rules of the task

GOAL
  session.har.txt is a capture of one browser session on shop.example.test: 400 entries, each with
  the request line, its headers and the response status and headers. One session cookie value was
  sent to a host that is not shop.example.test. Find it.

WHAT YOU HAVE
  session.har.txt    the capture (plain text; lines starting `>` are the request, `<` the response)
  The cookie is called sid. The server changes it once during the session. No network.

THE FINAL ANSWER
  answer.txt, with exactly these lines:
      entry: <the number of the entry that sent the cookie to the other host>
      cookie: <the cookie value that was sent>
  When you are sure, stop and reply in a few lines.
"
        .into(),
    );
    task(
        "http-har-leak",
        "a 2000-line capture where the leak is one query parameter of one third-party request, among decoys that \
         look the same",
        &[("RULES.md", rules), ("session.har.txt", leak(har().0))],
        vec![(
            1,
            "I think the shop leaked a session cookie to a third party during this capture. Read RULES.md first; find \
             the entry and the cookie value and write them in answer.txt."
                .into(),
        )],
        check_har,
        solution_har,
        None,
        || Ok(()),
    )
}

fn check_har(wd: &Path) -> Outcome {
    let a = keyed(wd, "answer.txt");
    Outcome::share(vec![line(&a, "entry", &HAR_ENTRY.to_string()), line(&a, "cookie", &har().1)])
}

fn solution_har(wd: &Path) {
    let _ = std::fs::write(wd.join("answer.txt"), format!("entry: {HAR_ENTRY}\ncookie: {}\n", har().1));
}

#[allow(dead_code)]
fn flush(mut s: impl Write) {
    let _ = s.flush();
}
