use super::*;
use httpmock::prelude::*;
use std::sync::atomic::{AtomicU64, Ordering};

fn ctx() -> ToolContext {
    ToolContext::new(std::env::temp_dir())
}

fn cfg(search: &str, url: Option<String>) -> WebConfig {
    WebConfig { search: search.into(), url, ..Default::default() }
}

const DDG_PAGE: &str = r#"<!DOCTYPE html><html><body>
<div class="serp__results"><div id="links" class="results">
  <div class="result results_links results_links_deep result--ad ">
<div class="links_main links_deep result__body">
  <h2 class="result__title"><a rel="nofollow" class="result__a" href="https://duckduckgo.com/y.js?ad_domain=shop.example&amp;u3=x">Buy Rust now</a></h2>
  <a class="result__snippet" href="https://duckduckgo.com/y.js?ad_domain=shop.example">An ad.</a>
</div>
  </div>
  <div class="result results_links results_links_deep web-result ">
<div class="links_main links_deep result__body">
  <h2 class="result__title">
    <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fratatui.rs%2Fconcepts%2F%3Fa%3D1%26b%3D2&amp;rut=abc123">Concepts | <b>Ratatui</b></a>
  </h2>
  <div class="result__extras"><div class="result__extras__url"><a class="result__url" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fratatui.rs%2F">ratatui.rs</a></div></div>
  <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fratatui.rs%2F"><b>Ratatui</b> is a Rust crate for cooking up terminal user interfaces &amp; more&#x27;s.</a>
  <div class="clear"></div>
</div>
  </div>
  <div class="result results_links results_links_deep web-result ">
<div class="links_main links_deep result__body">
  <h2 class="result__title"><a class="result__a" rel="nofollow" href="https://docs.rs/ratatui/latest/ratatui/">ratatui - Rust</a></h2>
  <a class="result__snippet" href="https://docs.rs/ratatui/">API documentation for the Rust `ratatui` crate.</a>
</div>
  </div>
</div></div></body></html>"#;

#[test]
fn duckduckgo_html_is_parsed_without_ads() {
    let h = parse_ddg(DDG_PAGE);
    assert_eq!(h.len(), 2, "{h:?}");
    assert_eq!(h[0].title, "Concepts | Ratatui");
    assert_eq!(h[0].url, "https://ratatui.rs/concepts/?a=1&b=2");
    assert_eq!(h[0].snippet, "Ratatui is a Rust crate for cooking up terminal user interfaces & more's.");
    assert_eq!(h[1].url, "https://docs.rs/ratatui/latest/ratatui/");
    assert_eq!(h[1].snippet, "API documentation for the Rust `ratatui` crate.");
}

#[test]
fn entities_and_sites() {
    assert_eq!(decode_entities("a &amp; b &lt;c&gt; &#39;d&#39; &#x41; &bogus;"), "a & b <c> 'd' A &bogus;");
    assert_eq!(with_sites("q", &[]), "q");
    assert_eq!(with_sites("q", &["docs.rs".into()]), "q site:docs.rs");
    assert_eq!(with_sites("q", &["a.io".into(), "b.io".into()]), "q (site:a.io OR site:b.io)");
    assert_eq!(domains_arg(&json!({"domains": ["https://docs.rs/", "  "]})), vec!["docs.rs"]);
}

#[tokio::test]
async fn duckduckgo_backend_posts_the_form() {
    let s = MockServer::start_async().await;
    let m = s
        .mock_async(|when, then| {
            when.method(POST).path("/html/").body_includes("q=ratatui+site%3Adocs.rs");
            then.status(200).header("content-type", "text/html").body(DDG_PAGE);
        })
        .await;
    let t = WebSearch::with_env(&cfg("duckduckgo", Some(s.base_url())), env_of(&[]));
    let out = t.run(json!({"query": "ratatui", "domains": ["docs.rs"], "max_results": 1}), &ctx()).await;
    m.assert_async().await;
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.starts_with("1 results for \"ratatui\" (duckduckgo):"), "{}", out.content);
    assert!(out.content.contains("1. Concepts | Ratatui\n   https://ratatui.rs/concepts/?a=1&b=2"));
    assert!(!out.content.contains("docs.rs/ratatui/latest"), "max_results holds");
}

#[tokio::test]
async fn duckduckgo_bot_check_is_explained() {
    let s = MockServer::start_async().await;
    s.mock_async(|when, then| {
        when.method(POST).path("/html/");
        then.status(202).body("<div class=\"anomaly-modal\">bots</div>");
    })
    .await;
    let t = WebSearch::with_env(&cfg("duckduckgo", Some(s.base_url())), env_of(&[]));
    let out = t.run(json!({"query": "x"}), &ctx()).await;
    assert!(out.is_error);
    assert!(out.content.contains("rate limit or a bot check"), "{}", out.content);
}

const SEARCH_DOWN: &str = "Web search is not available right now: do not retry it; go on with what you have, or open a URL you already know with web_fetch.";

/// A search tool whose clock is a number of seconds the test moves.
fn timed(t: &mut WebSearch) -> Arc<AtomicU64> {
    let secs = Arc::new(AtomicU64::new(0));
    let c = secs.clone();
    t.breaker.now = Arc::new(move || Duration::from_secs(c.load(Ordering::SeqCst)));
    secs
}

#[tokio::test]
async fn a_refused_search_says_not_to_retry() {
    let s = MockServer::start_async().await;
    s.mock_async(|when, then| {
        when.method(POST).path("/html/");
        then.status(429).body("");
    })
    .await;
    let t = WebSearch::with_env(&cfg("duckduckgo", Some(s.base_url())), env_of(&[]));
    let out = t.run(json!({"query": "x"}), &ctx()).await;
    assert!(out.is_error);
    assert!(out.content.contains("duckduckgo refused the search (HTTP 429"), "{}", out.content);
    assert!(out.content.ends_with(SEARCH_DOWN), "{}", out.content);
    // an unreachable backend counts too
    let t = WebSearch::with_env(&cfg("searxng", Some("http://127.0.0.1:1".into())), env_of(&[]));
    let out = t.run(json!({"query": "x"}), &ctx()).await;
    assert!(out.is_error && out.content.ends_with(SEARCH_DOWN), "{}", out.content);
    // a bad request is the query's trouble, not a refusal
    let s = MockServer::start_async().await;
    s.mock_async(|when, then| {
        when.method(GET).path("/search");
        then.status(400).body("bad query");
    })
    .await;
    let t = WebSearch::with_env(&cfg("searxng", Some(s.base_url())), env_of(&[]));
    let out = t.run(json!({"query": "x"}), &ctx()).await;
    assert!(out.is_error && !out.content.contains(SEARCH_DOWN), "{}", out.content);
}

#[tokio::test]
async fn two_refusals_in_a_row_open_the_breaker_and_time_closes_it() {
    let s = MockServer::start_async().await;
    let m = s
        .mock_async(|when, then| {
            when.method(POST).path("/html/");
            then.status(429).body("");
        })
        .await;
    let mut t = WebSearch::with_env(&cfg("duckduckgo", Some(s.base_url())), env_of(&[]));
    let secs = timed(&mut t);
    for _ in 0..2 {
        let out = t.run(json!({"query": "x"}), &ctx()).await;
        assert!(out.content.ends_with(SEARCH_DOWN), "{}", out.content);
    }
    assert_eq!(m.calls_async().await, 2);
    let out = t.run(json!({"query": "x"}), &ctx()).await;
    assert!(out.is_error);
    assert_eq!(out.content, BREAKER_SAID);
    assert!(out.content.contains("refused 2 requests in a row"));
    assert_eq!(m.calls_async().await, 2, "an open breaker makes no request");
    secs.store(119, Ordering::SeqCst);
    t.run(json!({"query": "x"}), &ctx()).await;
    assert_eq!(m.calls_async().await, 2, "still open at 119 s");
    secs.store(121, Ordering::SeqCst);
    let out = t.run(json!({"query": "x"}), &ctx()).await;
    assert_eq!(m.calls_async().await, 3, "closed after 120 s");
    assert!(out.content.ends_with(SEARCH_DOWN), "{}", out.content);
}

#[tokio::test]
async fn a_good_search_resets_the_count() {
    let s = MockServer::start_async().await;
    let t = WebSearch::with_env(&cfg("duckduckgo", Some(s.base_url())), env_of(&[]));
    for (code, body) in [(429, ""), (200, DDG_PAGE), (429, ""), (200, DDG_PAGE)] {
        let m = s
            .mock_async(move |when, then| {
                when.method(POST).path("/html/");
                then.status(code).body(body);
            })
            .await;
        let out = t.run(json!({"query": "x"}), &ctx()).await;
        // a refusal, a success, a refusal, a success: never two refusals in a row
        assert_eq!(out.is_error, code == 429, "{}", out.content);
        assert_ne!(out.content, BREAKER_SAID);
        m.delete_async().await;
    }
}

#[tokio::test]
async fn with_two_backends_the_breaker_counts_only_when_both_refused() {
    let s = MockServer::start_async().await;
    s.mock_async(|when, then| {
        when.method(POST).path("/search");
        then.status(401).body("no");
    })
    .await;
    let ok = s
        .mock_async(|when, then| {
            when.method(POST).path("/html/");
            then.status(200).body(DDG_PAGE);
        })
        .await;
    let mut t = WebSearch::with_env(&cfg("auto", Some(s.base_url())), env_of(&[("TAVILY_API_KEY", "bad")]));
    t.ddg_base = s.base_url();
    // the keyed backend refuses, DuckDuckGo answers: three calls, no breaker
    for _ in 0..3 {
        assert!(!t.run(json!({"query": "x"}), &ctx()).await.is_error);
    }
    assert_eq!(ok.calls_async().await, 3);
    ok.delete_async().await;
    let both = s
        .mock_async(|when, then| {
            when.method(POST).path("/html/");
            then.status(429).body("");
        })
        .await;
    for _ in 0..2 {
        let out = t.run(json!({"query": "x"}), &ctx()).await;
        assert!(
            out.content.contains("the fallback failed too") && out.content.ends_with(SEARCH_DOWN),
            "{}",
            out.content
        );
    }
    let out = t.run(json!({"query": "x"}), &ctx()).await;
    assert_eq!(out.content, BREAKER_SAID);
    assert_eq!(both.calls_async().await, 2);
}

#[tokio::test]
async fn a_404_fetch_says_not_to_guess_urls_and_other_errors_stay_as_they_were() {
    let s = MockServer::start_async().await;
    for (path, code) in [("/gone", 410), ("/missing", 404), ("/boom", 500), ("/nope", 403)] {
        s.mock_async(move |when, then| {
            when.method(GET).path(path);
            then.status(code).body("x");
        })
        .await;
    }
    let t = WebFetch::new(&WebConfig::default());
    let sentence = " The page does not exist: do not guess URLs; use one that a search result or a page gave you.";
    let out = t.run(json!({"url": s.url("/missing")}), &ctx()).await;
    assert!(
        out.is_error && out.content.ends_with(&format!("HTTP 404 Not Found for {}{sentence}", s.url("/missing"))),
        "{}",
        out.content
    );
    let out = t.run(json!({"url": s.url("/gone")}), &ctx()).await;
    assert!(out.is_error && out.content.ends_with(sentence), "{}", out.content);
    let out = t.run(json!({"url": s.url("/boom")}), &ctx()).await;
    assert_eq!(out.content, format!("web_fetch: HTTP 500 Internal Server Error for {}", s.url("/boom")));
    let out = t.run(json!({"url": s.url("/nope")}), &ctx()).await;
    assert_eq!(out.content, format!("web_fetch: HTTP 403 Forbidden for {}", s.url("/nope")));
}
#[tokio::test]
async fn tavily_backend() {
    let s = MockServer::start_async().await;
    let m = s
        .mock_async(|when, then| {
            when.method(POST)
                .path("/search")
                .header("authorization", "Bearer tv-key")
                .json_body_includes(r#"{"query": "rust 2024", "max_results": 3, "include_domains": ["rust-lang.org"]}"#);
            then.status(200).json_body(json!({
                "query": "rust 2024",
                "results": [
                    {"title": "Rust 2024 | Edition Guide", "url": "https://doc.rust-lang.org/edition-guide/rust-2024/", "content": "The Rust 2024 Edition was stabilised in 1.85.", "score": 0.9},
                    {"title": "No url", "content": "dropped"}
                ]
            }));
        })
        .await;
    let t = WebSearch::with_env(&cfg("tavily", Some(s.base_url())), env_of(&[("TAVILY_API_KEY", "tv-key")]));
    let out = t.run(json!({"query": "rust 2024", "max_results": 3, "domains": ["rust-lang.org"]}), &ctx()).await;
    m.assert_async().await;
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("(tavily)"));
    assert!(out.content.contains(
        "1. Rust 2024 | Edition Guide\n   https://doc.rust-lang.org/edition-guide/rust-2024/\n   The Rust 2024 Edition"
    ));
    assert!(!out.content.contains("No url"));
}

#[tokio::test]
async fn brave_backend() {
    let s = MockServer::start_async().await;
    let m = s
        .mock_async(|when, then| {
            when.method(GET)
                .path("/res/v1/web/search")
                .query_param("q", "tokio select")
                .query_param("count", "8")
                .header("x-subscription-token", "br-key");
            then.status(200).json_body(json!({
                "type": "search",
                "web": {"type": "search", "results": [
                    {"title": "select! in <strong>tokio</strong>", "url": "https://docs.rs/tokio/latest/tokio/macro.select.html", "description": "Waits on multiple concurrent branches, returning when the <strong>first</strong> branch completes."}
                ]}
            }));
        })
        .await;
    let t = WebSearch::with_env(&cfg("brave", Some(s.base_url())), env_of(&[("BRAVE_SEARCH_API_KEY", "br-key")]));
    let out = t.run(json!({"query": "tokio select"}), &ctx()).await;
    m.assert_async().await;
    assert!(out.content.contains("1. select! in tokio\n   https://docs.rs/tokio/latest/tokio/macro.select.html\n   Waits on multiple concurrent branches, returning when the first branch completes."), "{}", out.content);
}

#[tokio::test]
async fn exa_backend() {
    let s = MockServer::start_async().await;
    let m = s
        .mock_async(|when, then| {
            when.method(POST).path("/search").header("x-api-key", "exa-key").json_body_includes(r#"{"numResults": 2}"#);
            then.status(200).json_body(json!({
                "requestId": "r1",
                "results": [
                    {"id": "1", "title": "Serde", "url": "https://serde.rs/", "highlights": ["Serde is a framework", "for serializing"], "publishedDate": null}
                ]
            }));
        })
        .await;
    let t = WebSearch::with_env(&cfg("exa", Some(s.base_url())), env_of(&[("EXA_API_KEY", "exa-key")]));
    let out = t.run(json!({"query": "serde", "max_results": 2}), &ctx()).await;
    m.assert_async().await;
    assert!(
        out.content.contains("1. Serde\n   https://serde.rs/\n   Serde is a framework … for serializing"),
        "{}",
        out.content
    );
}

#[tokio::test]
async fn searxng_backend_and_its_address() {
    let s = MockServer::start_async().await;
    let m = s
        .mock_async(|when, then| {
            when.method(GET).path("/search").query_param("format", "json").query_param("q", "nexo");
            then.status(200).json_body(json!({
                "query": "nexo",
                "results": [{"title": "Nexo", "url": "https://example.org/nexo", "content": "A harness.", "engine": "bing"}]
            }));
        })
        .await;
    let t = WebSearch::with_env(&cfg("searxng", Some(format!("{}/", s.base_url()))), env_of(&[]));
    let out = t.run(json!({"query": "nexo"}), &ctx()).await;
    m.assert_async().await;
    assert!(out.content.contains("(searxng)") && out.content.contains("A harness."), "{}", out.content);
    let none = WebSearch::with_env(&cfg("searxng", None), env_of(&[]));
    let out = none.run(json!({"query": "nexo"}), &ctx()).await;
    assert!(out.is_error && out.content.contains("[web] url"), "{}", out.content);
}

#[tokio::test]
async fn auto_takes_the_keyed_backend_and_falls_back_to_duckduckgo() {
    let s = MockServer::start_async().await;
    let tav = s
        .mock_async(|when, then| {
            when.method(POST).path("/search");
            then.status(401).body(r#"{"detail": {"error": "Unauthorized: missing or invalid API key."}}"#);
        })
        .await;
    let ddg = s
        .mock_async(|when, then| {
            when.method(POST).path("/html/");
            then.status(200).body(DDG_PAGE);
        })
        .await;
    let mut t = WebSearch::with_env(&cfg("auto", Some(s.base_url())), env_of(&[("TAVILY_API_KEY", "bad")]));
    t.ddg_base = s.base_url();
    assert_eq!(t.choose().unwrap(), (Backend::Tavily, true));
    let out = t.run(json!({"query": "ratatui"}), &ctx()).await;
    tav.assert_async().await;
    ddg.assert_async().await;
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.starts_with("(tavily: HTTP 401"), "{}", out.content);
    assert!(out.content.contains("fell back to duckduckgo"));
    assert!(out.content.contains("Concepts | Ratatui"));
    // no key, no address: DuckDuckGo; an address alone: SearXNG
    assert_eq!(WebSearch::with_env(&cfg("auto", None), env_of(&[])).choose().unwrap(), (Backend::DuckDuckGo, false));
    assert_eq!(
        WebSearch::with_env(&cfg("auto", Some("http://s".into())), env_of(&[])).choose().unwrap(),
        (Backend::Searxng, true)
    );
    assert!(WebSearch::with_env(&cfg("google", None), env_of(&[])).choose().is_err());
}

#[tokio::test]
async fn a_named_backend_without_its_key_says_which_variable() {
    let t = WebSearch::with_env(&cfg("brave", None), env_of(&[]));
    let out = t.run(json!({"query": "x"}), &ctx()).await;
    assert!(out.is_error);
    assert!(out.content.contains("$BRAVE_API_KEY or $BRAVE_SEARCH_API_KEY"), "{}", out.content);
    let mut c = cfg("tavily", None);
    c.api_key_env = Some("MY_TAVILY".into());
    let out = WebSearch::with_env(&c, env_of(&[])).run(json!({"query": "x"}), &ctx()).await;
    assert!(out.content.contains("$MY_TAVILY"), "{}", out.content);
    let out = WebSearch::with_env(&c, env_of(&[])).run(json!({}), &ctx()).await;
    assert!(out.is_error && out.content.contains("`query` is required"));
}

const ARTICLE: &str = r#"<!doctype html><html><head><title>Async &amp; Await</title>
<style>body{color:red}</style><script>alert("x")</script></head>
<body><nav><a href="/">Home</a> | <a href="/blog">Blog</a></nav>
<main><h1>Async &amp; Await</h1>
<p>Read <a href="../guide/intro.html">the guide</a> first.</p>
<h2>Example</h2>
<pre><code>async fn main() {}</code></pre>
<ul><li>one</li><li>two</li></ul>
<form><input name="q"><button>Go</button></form>
</main><noscript>Enable JS</noscript></body></html>"#;

#[test]
fn html_becomes_markdown_with_absolute_links() {
    let base = Url::parse("https://example.org/book/ch1/page.html").unwrap();
    let md = html_to_markdown(ARTICLE, Some(&base));
    assert!(md.starts_with("# Async & Await"), "{md}");
    assert_eq!(md.matches("Async & Await").count(), 1, "the title is not repeated: {md}");
    assert!(md.contains("[the guide](https://example.org/book/guide/intro.html)"), "{md}");
    assert!(md.contains("## Example"), "{md}");
    assert!(md.contains("async fn main() {}"), "{md}");
    assert!(md.contains("one") && md.contains("two"));
    for gone in ["alert", "color:red", "Blog", "Enable JS", "Go"] {
        assert!(!md.contains(gone), "{gone} left in: {md}");
    }
}

#[test]
fn pages_are_cut_in_characters() {
    let (p, n) = page("héllo wörld", 0, 5);
    assert_eq!(p, "héllo");
    assert_eq!(n.as_deref(), Some("… 6 more characters; call again with start=5"));
    let (p, n) = page("héllo wörld", 5, 100);
    assert_eq!(p, " wörld");
    assert!(n.is_none());
    let (_, n) = page("abc", 9, 5);
    assert!(n.unwrap().contains("past the end"));
}

#[test]
fn redirect_blocks_private_hosts() {
    let u = |s: &str| Url::parse(s).unwrap();
    for s in [
        "http://127.0.0.1:1/",
        "http://169.254.169.254/",
        "http://[::1]/",
        "http://10.0.0.1/",
        "http://localhost/",
        "http://[::ffff:192.168.1.1]/",
        "http://[fe80::1]/",
        "http://0.1.2.3/",
        "http://[::127.0.0.1]/",
        "http://[::169.254.169.254]/",
        "http://[64:ff9b::7f00:1]/",
        "http://[64:ff9b::10.0.0.1]/",
        "http://[64:ff9b:1::8.8.8.8]/",
        "http://[2002:7f00:1::]/",
        "http://[2001:0:4136:e378:8000:63bf:80ff:fffe]/",
        "http://[fec0::1]/",
    ] {
        assert!(private_host(&u(s)), "{s}");
        assert!(!redirect_ok(&[u("https://docs.rs/")], &u(s)), "{s}");
    }
    assert!(!private_host(&u("https://docs.rs/")));
    assert!(!private_host(&u("http://8.8.8.8/")));
    for s in ["http://[2606:4700::1111]/", "http://[64:ff9b::808:808]/", "http://[2002:808:808::]/"] {
        assert!(!private_host(&u(s)), "{s}");
    }
    assert!(redirect_ok(&[u("https://docs.rs/")], &u("https://crates.io/")));
    assert!(redirect_ok(&[u("http://127.0.0.1:8080/")], &u("http://127.0.0.1:8080/b")), "a local start may stay local");
}

#[test]
fn a_name_is_refused_before_connecting_when_it_resolves_to_a_private_address() {
    let a = |s: &str| -> std::net::SocketAddr { s.parse().unwrap() };
    let public = vec![a("93.184.216.34:0"), a("[2606:2800:220:1::1]:0")];
    assert_eq!(public_only("example.com", public.clone()), Ok(public));
    for addrs in [
        vec![a("127.0.0.1:0")],
        vec![a("93.184.216.34:0"), a("10.0.0.5:0")],
        vec![a("[::1]:0")],
        vec![a("[::ffff:169.254.169.254]:0")],
        vec![a("[64:ff9b::c0a8:101]:0")],
    ] {
        let e = public_only("rebind.example", addrs).unwrap_err();
        assert!(e.contains("local or private"), "{e}");
    }
    let local = vec![a("127.0.0.1:0")];
    assert_eq!(public_only("localhost", local.clone()), Ok(local.clone()));
    assert_eq!(public_only("app.localhost.", local.clone()), Ok(local));
}

#[test]
fn kinds_by_type_and_bytes() {
    assert_eq!(kind_of("text/html; charset=utf-8", b""), Kind::Html);
    assert_eq!(kind_of("application/json", b"{}"), Kind::Text);
    assert_eq!(kind_of("text/markdown", b"# a"), Kind::Text);
    assert_eq!(kind_of("", b"<!DOCTYPE html><html>"), Kind::Html);
    assert_eq!(kind_of("application/pdf", b"%PDF-1.7"), Kind::Refused("application/pdf".into()));
    assert!(matches!(kind_of("", &[0u8, 159, 146, 150, 0, 0]), Kind::Refused(_)));
}

#[tokio::test]
async fn fetch_reads_html_text_and_pages() {
    let s = MockServer::start_async().await;
    s.mock_async(|when, then| {
        when.method(GET).path("/book/ch1/page.html");
        then.status(200).header("content-type", "text/html; charset=utf-8").body(ARTICLE);
    })
    .await;
    s.mock_async(|when, then| {
        when.method(GET).path("/data.json");
        then.status(200).header("content-type", "application/json").body(r#"{"a": 1}"#);
    })
    .await;
    s.mock_async(|when, then| {
        when.method(GET).path("/long.txt");
        then.status(200).header("content-type", "text/plain").body("0123456789".repeat(10));
    })
    .await;
    let t = WebFetch::new(&WebConfig::default());
    let out = t.run(json!({"url": s.url("/book/ch1/page.html")}), &ctx()).await;
    assert!(!out.is_error, "{}", out.content);
    assert!(
        out.content.starts_with(&format!("URL: {}\n\n# Async & Await", s.url("/book/ch1/page.html"))),
        "{}",
        out.content
    );
    assert!(out.content.contains(&format!("[the guide]({})", s.url("/book/guide/intro.html"))), "{}", out.content);
    let out = t.run(json!({"url": s.url("/data.json")}), &ctx()).await;
    assert!(out.content.ends_with(r#"{"a": 1}"#), "{}", out.content);
    let out = t.run(json!({"url": s.url("/long.txt"), "max_chars": 30}), &ctx()).await;
    assert!(out.content.ends_with("… 70 more characters; call again with start=30"), "{}", out.content);
    let out = t.run(json!({"url": s.url("/long.txt"), "start": 95}), &ctx()).await;
    assert!(out.content.ends_with("\n\n56789"), "{}", out.content);
}

#[tokio::test]
async fn fetch_refuses_what_it_cannot_read() {
    let s = MockServer::start_async().await;
    s.mock_async(|when, then| {
        when.method(GET).path("/missing");
        then.status(404).body("not here");
    })
    .await;
    s.mock_async(|when, then| {
        when.method(GET).path("/paper.pdf");
        then.status(200).header("content-type", "application/pdf").body("%PDF-1.7 ...");
    })
    .await;
    let t = WebFetch::new(&WebConfig::default());
    let out = t.run(json!({"url": s.url("/missing")}), &ctx()).await;
    assert!(out.is_error && out.content.contains("HTTP 404 Not Found"), "{}", out.content);
    let out = t.run(json!({"url": s.url("/paper.pdf")}), &ctx()).await;
    assert!(out.is_error && out.content.contains("application/pdf, not text"), "{}", out.content);
    let out = t.run(json!({"url": "file:///etc/passwd"}), &ctx()).await;
    assert!(out.is_error && out.content.contains("only http and https"), "{}", out.content);
    let out = t.run(json!({"url": "not a url"}), &ctx()).await;
    assert!(out.is_error && out.content.contains("is not a URL"), "{}", out.content);
}

/// Rules that deny `deny`, allow `allow`, and ask about any other host.
fn rules(deny: &'static str, allow: &'static str) -> nexo_core::tool::PolicyCheck {
    Arc::new(move |tool: &str, kind, args: &Value| {
        assert_eq!((tool, kind), ("web_fetch", ToolKind::Network));
        let host = Url::parse(args["url"].as_str().unwrap()).unwrap().host_str().unwrap().to_string();
        if host == deny {
            Check::Deny
        } else if host == allow {
            Check::Allow
        } else {
            Check::Ask
        }
    })
}

#[test]
fn a_redirect_is_checked_by_the_rules_as_if_its_target_were_asked_for() {
    let u = |s: &str| Url::parse(s).unwrap();
    let docs = [u("https://docs.rs/tokio")];
    let r = rules("evil.example", "crates.io");
    assert_eq!(hop(&docs, &u("https://docs.rs/tokio/latest/"), Some(&r)), Hop::Follow, "same site");
    assert_eq!(hop(&docs, &u("https://crates.io/x"), Some(&r)), Hop::Follow, "allowed by a rule");
    assert!(matches!(hop(&docs, &u("https://evil.example/?k=1"), Some(&r)), Hop::Refuse(w) if w.contains("denies")));
    assert!(
        matches!(hop(&docs, &u("https://other.example/"), Some(&r)), Hop::Stop(s) if s.contains("call web_fetch with that URL"))
    );
    assert_eq!(hop(&docs, &u("https://other.example/"), None), Hop::Follow, "no rules, only the address rule");
    // the address rule comes first, whatever the rules allow
    let all = rules("", "169.254.169.254");
    assert!(
        matches!(hop(&docs, &u("http://169.254.169.254/latest/"), Some(&all)), Hop::Refuse(w) if w.contains("private"))
    );
    let six: Vec<Url> = (0..6).map(|i| u(&format!("https://docs.rs/{i}"))).collect();
    assert!(matches!(hop(&six, &u("https://docs.rs/7"), None), Hop::Refuse(w) if w.contains("more than 5")));
}

#[tokio::test]
async fn fetch_follows_a_redirect_only_where_the_rules_would_let_it_go() {
    let s = MockServer::start_async().await;
    let other = format!("http://localhost:{}/far", s.port());
    s.mock_async(|when, then| {
        when.method(GET).path("/old");
        then.status(301).header("location", "/new");
    })
    .await;
    s.mock_async(|when, then| {
        when.method(GET).path("/new");
        then.status(200).header("content-type", "text/plain").body("the new page");
    })
    .await;
    s.mock_async(|when, then| {
        when.method(GET).path("/away");
        then.status(302).header("location", other.as_str());
    })
    .await;
    let far = s
        .mock_async(|when, then| {
            when.method(GET).path("/far");
            then.status(200).header("content-type", "text/plain").body("the far page");
        })
        .await;
    let t = WebFetch::new(&WebConfig::default());
    let asks = ToolContext { check: Some(rules("", "")), ..ctx() };
    let out = t.run(json!({"url": s.url("/old")}), &asks).await;
    assert!(!out.is_error && out.content.ends_with("the new page"), "{}", out.content);
    assert!(out.content.contains(&format!("(redirected from {})", s.url("/old"))), "{}", out.content);
    // another site the rules do not cover: not read, its address said
    let out = t.run(json!({"url": s.url("/away")}), &asks).await;
    assert!(!out.is_error && out.content.contains(&other) && !out.content.contains("the far page"), "{}", out.content);
    far.assert_calls_async(0).await;
    let denies = ToolContext { check: Some(rules("localhost", "")), ..ctx() };
    let out = t.run(json!({"url": s.url("/away")}), &denies).await;
    assert!(out.is_error && out.content.contains("a permission rule denies"), "{}", out.content);
    far.assert_calls_async(0).await;
    let allows = ToolContext { check: Some(rules("", "localhost")), ..ctx() };
    let out = t.run(json!({"url": s.url("/away")}), &allows).await;
    assert!(!out.is_error && out.content.ends_with("the far page"), "{}", out.content);
}

#[test]
fn summaries_and_registration() {
    let f = WebFetch::new(&WebConfig::default());
    assert_eq!(
        f.summary(&json!({"url": "https://docs.rs/tokio/latest/tokio/?x=1"})),
        "web_fetch: docs.rs/tokio/latest/tokio/"
    );
    assert_eq!(f.summary(&json!({"url": "https://ratatui.rs/", "start": 2000})), "web_fetch: ratatui.rs (from 2000)");
    let s = WebSearch::new(&WebConfig::default());
    assert_eq!(
        s.summary(&json!({"query": "tokio select", "domains": ["docs.rs"]})),
        "web_search: tokio select (on docs.rs)"
    );
    assert_eq!(s.kind(), ToolKind::Search);
    assert_eq!(f.kind(), ToolKind::Network);
    assert_eq!(tools(&WebConfig::default()).len(), 2);
    assert!(tools(&WebConfig { enabled: false, ..Default::default() }).is_empty());
}
