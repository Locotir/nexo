use super::*;
use httpmock::prelude::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

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

/// A search tool whose clock is a number of seconds the test moves.
fn timed(t: &mut WebSearch) -> Arc<AtomicU64> {
    let secs = Arc::new(AtomicU64::new(0));
    let c = secs.clone();
    t.breaker.now = Arc::new(move || Duration::from_secs(c.load(Ordering::SeqCst)));
    secs
}

#[tokio::test]
async fn a_refused_search_says_how_many_refusals_in_a_row() {
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
    assert!(
        out.content.contains("Refusal 1 in a row") && out.content.contains("web_fetch still opens"),
        "{}",
        out.content
    );
    // an unreachable backend counts too
    let t = WebSearch::with_env(&cfg("searxng", Some("http://127.0.0.1:1".into())), env_of(&[]));
    let out = t.run(json!({"query": "x"}), &ctx()).await;
    assert!(out.is_error && out.content.contains("Refusal 1 in a row"), "{}", out.content);
    // a bad request is the query's trouble, not a refusal
    let s = MockServer::start_async().await;
    s.mock_async(|when, then| {
        when.method(GET).path("/search");
        then.status(400).body("bad query");
    })
    .await;
    let t = WebSearch::with_env(&cfg("searxng", Some(s.base_url())), env_of(&[]));
    let out = t.run(json!({"query": "x"}), &ctx()).await;
    assert!(out.is_error && !out.content.contains("Refusal"), "{}", out.content);
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
    for i in 0..2 {
        let out = t.run(json!({"query": "x"}), &ctx()).await;
        assert!(out.content.contains(&format!("Refusal {} in a row", i + 1)), "{}", out.content);
        assert_eq!(out.content.contains("the next 120 s"), i == 1, "{}", out.content);
    }
    assert_eq!(m.calls_async().await, 2);
    let out = t.run(json!({"query": "x"}), &ctx()).await;
    assert!(out.is_error);
    assert!(out.content.starts_with("web_search: not asked:"), "{}", out.content);
    assert!(out.content.contains("refused 2 requests in a row"));
    assert_eq!(m.calls_async().await, 2, "an open breaker makes no request");
    secs.store(119, Ordering::SeqCst);
    t.run(json!({"query": "x"}), &ctx()).await;
    assert_eq!(m.calls_async().await, 2, "still open at 119 s");
    secs.store(121, Ordering::SeqCst);
    let out = t.run(json!({"query": "x"}), &ctx()).await;
    assert_eq!(m.calls_async().await, 3, "closed after 120 s");
    assert!(out.content.contains("Refusal 3 in a row") && out.content.contains("the next 120 s"), "{}", out.content);
}

#[tokio::test]
async fn an_open_breaker_says_when_it_asks_again() {
    let s = MockServer::start_async().await;
    s.mock_async(|when, then| {
        when.method(POST).path("/html/");
        then.status(429).body("");
    })
    .await;
    let mut t = WebSearch::with_env(&cfg("duckduckgo", Some(s.base_url())), env_of(&[]));
    let secs = timed(&mut t);
    for _ in 0..2 {
        t.run(json!({"query": "x"}), &ctx()).await;
    }
    let out = t.run(json!({"query": "x"}), &ctx()).await;
    assert!(out.is_error);
    for want in ["the next 120 s", "last: duckduckgo", "asks it again"] {
        assert!(out.content.contains(want), "{}", out.content);
    }
    for gone in ["this session", "Do not call"] {
        assert!(!out.content.contains(gone), "{}", out.content);
    }
    secs.store(119, Ordering::SeqCst);
    let out = t.run(json!({"query": "x"}), &ctx()).await;
    assert!(out.content.contains("the next 1 s"), "{}", out.content);
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
        assert!(!out.content.starts_with("web_search: not asked"), "{}", out.content);
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
    for i in 0..2 {
        let out = t.run(json!({"query": "x"}), &ctx()).await;
        assert!(
            out.content.contains("the fallback failed too")
                && out.content.contains(&format!("Refusal {} in a row", i + 1)),
            "{}",
            out.content
        );
    }
    let out = t.run(json!({"query": "x"}), &ctx()).await;
    assert!(out.content.starts_with("web_search: not asked:"), "{}", out.content);
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
async fn searxng_says_which_engines_failed_instead_of_no_results() {
    let s = MockServer::start_async().await;
    let none = s
        .mock_async(|when, then| {
            when.method(GET).path("/search").query_param("q", "nothing");
            then.status(200).json_body(json!({"results": [], "unresponsive_engines": [["google", "CAPTCHA"]]}));
        })
        .await;
    s.mock_async(|when, then| {
        when.method(GET).path("/search").query_param("q", "some");
        then.status(200).json_body(json!({
            "results": [{"title": "Nexo", "url": "https://example.org/nexo", "content": "A harness."}],
            "unresponsive_engines": [["bing", "timeout"]]
        }));
    })
    .await;
    let t = WebSearch::with_env(&cfg("searxng", Some(s.base_url())), env_of(&[]));
    let out = t.run(json!({"query": "nothing"}), &ctx()).await;
    none.assert_async().await;
    assert!(out.is_error, "{}", out.content);
    assert!(
        out.content.contains("searxng: no results; these engines did not answer: google (CAPTCHA)"),
        "{}",
        out.content
    );
    assert!(!out.content.contains("no results for") && !out.content.contains("Refusal"), "{}", out.content);
    let out = t.run(json!({"query": "some"}), &ctx()).await;
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("A harness."), "{}", out.content);
    assert!(out.content.contains("these engines did not answer: bing (timeout)"), "{}", out.content);
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

#[tokio::test]
async fn fetch_decodes_the_charset_the_header_names() {
    let s = MockServer::start_async().await;
    s.mock_async(|when, then| {
        when.method(GET).path("/latin1.txt");
        then.status(200).header("content-type", "text/plain; charset=ISO-8859-1").body(b"caf\xE9 cr\xE8me");
    })
    .await;
    let t = WebFetch::new(&WebConfig::default());
    let out = t.run(json!({"url": s.url("/latin1.txt")}), &ctx()).await;
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("café crème"), "{}", out.content);
    assert!(!out.content.contains('\u{FFFD}'), "{}", out.content);
    assert!(out.content.contains("(decoded as windows-1252)"), "{}", out.content);
}

#[tokio::test]
async fn fetch_decodes_the_charset_a_meta_tag_names_when_the_header_does_not() {
    let s = MockServer::start_async().await;
    // 0x80 is the euro sign in windows-1252 and a control character in latin-1, so only the real label passes
    let cp1252 = b"<html><head><meta charset=\"windows-1252\"><title>Prix</title></head><body><p>caf\xE9 \x80 5</p></body></html>".to_vec();
    s.mock_async(move |when, then| {
        when.method(GET).path("/meta.html");
        then.status(200).header("content-type", "text/html").body(cp1252);
    })
    .await;
    let sjis = b"<html><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=shift_jis\"></head><body><p>\x93\xFA\x96\x7B\x8C\xEA</p></body></html>".to_vec();
    s.mock_async(move |when, then| {
        when.method(GET).path("/sjis.html");
        then.status(200).header("content-type", "text/html").body(sjis);
    })
    .await;
    let t = WebFetch::new(&WebConfig::default());
    let out = t.run(json!({"url": s.url("/meta.html")}), &ctx()).await;
    assert!(out.content.contains("café € 5"), "{}", out.content);
    let out = t.run(json!({"url": s.url("/sjis.html")}), &ctx()).await;
    assert!(out.content.contains("日本語"), "{}", out.content);
    assert!(out.content.contains("(decoded as Shift_JIS)"), "{}", out.content);
}

#[test]
fn decode_prefers_a_byte_order_mark_then_the_header_then_the_meta_and_falls_back_to_utf8_then_windows_1252() {
    // a UTF-16LE mark beats a header that says otherwise
    let (text, name) = decode("text/plain; charset=iso-8859-1", b"\xFF\xFEh\0i\0", false, false);
    assert_eq!((&*text, name), ("hi", Some("UTF-16LE")));
    // the header beats the meta
    let (text, _) = decode("text/html; charset=iso-8859-1", b"<meta charset=\"utf-8\">\xE9", true, false);
    assert!(text.ends_with('é'), "{text}");
    // a meta is read only for HTML, and only in the first KiB
    let (text, name) = decode("text/plain", b"<meta charset=\"iso-8859-1\">\xE9", false, false);
    assert!(text.ends_with('é') && name.is_some(), "{text}");
    let late = [vec![b' '; 2000], b"<meta charset=\"iso-8859-1\">\xE9".to_vec()].concat();
    assert!(decode("text/html", &late, true, false).1.is_some());
    // an unlabeled page that is valid UTF-8 stays UTF-8
    assert_eq!(decode("text/plain", "café".as_bytes(), false, false).1, None);
    // a label nobody knows is ignored: the UTF-8 default reads the body
    let (text, name) = decode("text/plain; charset=made-up-9", "café".as_bytes(), false, false);
    assert_eq!((&*text, name), ("café", None));
    // a label that means the replacement encoding is ignored too: it would turn the page into one U+FFFD
    let (text, name) = decode("text/plain; charset=iso-2022-kr", b"hello", false, false);
    assert_eq!((&*text, name), ("hello", None));
    let (text, name) = decode("text/html", b"<meta charset=\"hz-gb-2312\">hello", true, false);
    assert!(text.ends_with("hello") && name.is_none(), "{text}");
    // a page that says utf-8 stays on the borrowed fast path
    assert!(matches!(
        decode("text/html; charset=\"UTF-8\"", b"<p>ok</p>", true, false).0,
        std::borrow::Cow::Borrowed(_)
    ));
}

#[test]
fn fetch_decodes_an_unlabeled_page_that_is_not_utf8_as_windows_1252() {
    let (text, note) = decode("text/html", b"caf\xE9", false, false);
    assert_eq!(&*text, "café");
    assert!(note.is_some_and(|n| n.contains("windows-1252")), "{note:?}");
    // UTF-8 cut inside its last character is not windows-1252
    let (text, note) = decode("text/html", b"caf\xC3\xA9 caf\xC3", false, true);
    assert_eq!(&*text, "café caf");
    assert_eq!(note, None);
    // a whole page that ends in a lone lead byte is not UTF-8 cut short: it is a legacy page
    let (text, note) = decode("text/html", b"caf\xC3\xA9 caf\xC3", false, false);
    assert_eq!(&*text, "caf\u{c3}\u{a9} caf\u{c3}");
    assert!(note.is_some_and(|n| n.contains("windows-1252")));
}

#[tokio::test]
async fn a_body_cut_short_keeps_what_arrived() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = sock.read(&mut buf).await;
        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 100000\r\n\r\n";
        sock.write_all(head.as_bytes()).await.unwrap();
        sock.write_all(&[b'a'; 4096]).await.unwrap();
        // the connection closes with the rest of the body unsent
    });
    let t = WebFetch::new(&WebConfig::default());
    let out = t.run(json!({"url": format!("http://{addr}/part.txt")}), &ctx()).await;
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains(&"a".repeat(4096)), "{}", out.content);
    assert!(out.content.contains("connection ended after 4 KB"), "{}", out.content);
    assert!(out.content.contains("only its start was read"), "{}", out.content);
}

#[tokio::test]
async fn a_429_fetch_shows_retry_after_and_the_json_error_body() {
    let s = MockServer::start_async().await;
    s.mock_async(|when, then| {
        when.method(GET).path("/api");
        then.status(429)
            .header("content-type", "application/json")
            .header("retry-after", " 30 ")
            .body(r#"{"error": "rate limit: 5 requests per minute"}"#);
    })
    .await;
    let t = WebFetch::new(&WebConfig::default());
    let out = t.run(json!({"url": s.url("/api")}), &ctx()).await;
    assert!(out.is_error, "{}", out.content);
    assert_eq!(
        out.content,
        format!(
            "web_fetch: HTTP 429 Too Many Requests for {}. Retry-After: 30.\n\nResponse body: {{\"error\": \"rate limit: 5 requests per minute\"}}",
            s.url("/api")
        )
    );
}

#[tokio::test]
async fn a_403_fetch_shows_the_text_of_an_html_challenge_page_not_its_scripts() {
    let s = MockServer::start_async().await;
    let page = format!(
        "<html><head><title>Just a moment...</title><style>{}</style><script>var secret = 1;</script></head>\
         <body><h1>Checking your browser</h1><p>{}</p></body></html>",
        "x{}".repeat(50),
        "Enable JavaScript and cookies to continue. ".repeat(40)
    );
    s.mock_async(move |when, then| {
        when.method(GET).path("/blocked");
        then.status(403).header("content-type", "text/html; charset=utf-8").body(page);
    })
    .await;
    let t = WebFetch::new(&WebConfig::default());
    let out = t.run(json!({"url": s.url("/blocked")}), &ctx()).await;
    assert!(out.is_error && out.content.contains("HTTP 403 Forbidden"), "{}", out.content);
    assert!(out.content.contains("Response body (first 500 characters): # Just a moment..."), "{}", out.content);
    assert!(out.content.contains("Checking your browser"), "{}", out.content);
    assert!(!out.content.contains("secret"), "{}", out.content);
    let shown = out.content.split("characters): ").nth(1).unwrap();
    assert_eq!(shown.chars().count(), 500, "{shown}");
}

#[tokio::test]
async fn a_failed_fetch_with_a_binary_body_shows_no_body() {
    let s = MockServer::start_async().await;
    s.mock_async(|when, then| {
        when.method(GET).path("/img");
        then.status(500).header("content-type", "image/png").body(b"\x89PNG\r\n");
    })
    .await;
    let t = WebFetch::new(&WebConfig::default());
    let out = t.run(json!({"url": s.url("/img")}), &ctx()).await;
    assert_eq!(out.content, format!("web_fetch: HTTP 500 Internal Server Error for {}", s.url("/img")));
}

#[tokio::test]
async fn a_pdf_fetch_says_what_the_file_is() {
    let s = MockServer::start_async().await;
    let m = s
        .mock_async(|when, then| {
            when.method(GET).path("/paper.pdf");
            then.status(200).header("content-type", "application/pdf").body("%PDF-1.7 ...");
        })
        .await;
    let t = WebFetch::new(&WebConfig::default());
    let out = t.run(json!({"url": s.url("/paper.pdf")}), &ctx()).await;
    m.assert_async().await;
    assert!(out.is_error, "{}", out.content);
    assert_eq!(
        out.content,
        format!(
            "web_fetch: {} is application/pdf, not text; web_fetch reads HTML, text, Markdown and JSON. The file is a PDF (12 bytes).",
            s.url("/paper.pdf")
        )
    );
}

#[tokio::test]
async fn a_file_refused_by_its_headers_is_not_downloaded() {
    // a server that promises a large PDF and never sends it: reading the body would wait for the timeout
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = sock.read(&mut buf).await;
        let head = "HTTP/1.1 200 OK\r\ncontent-type: application/pdf\r\ncontent-length: 50000000\r\n\r\n%PDF";
        sock.write_all(head.as_bytes()).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    });
    let t = WebFetch::new(&WebConfig::default());
    let cx = ctx();
    let run = t.run(json!({"url": format!("http://{addr}/big.pdf")}), &cx);
    let out = tokio::time::timeout(std::time::Duration::from_secs(10), run).await.expect("the body was waited for");
    assert!(out.is_error && out.content.contains("The file is a PDF (50000000 bytes)."), "{}", out.content);
    assert!(
        out.content.starts_with("web_fetch: http://") && !out.content.contains("web_fetch: web_fetch"),
        "{}",
        out.content
    );
}

#[tokio::test]
async fn an_error_page_cut_inside_an_unclosed_script_shows_no_script_text() {
    let s = MockServer::start_async().await;
    let page = format!("<html><body><p>Access denied</p><script>var secret = \"{}\";", "x".repeat(20_000));
    s.mock_async(move |when, then| {
        when.method(GET).path("/cut");
        then.status(403).header("content-type", "text/html").body(page);
    })
    .await;
    let t = WebFetch::new(&WebConfig::default());
    let out = t.run(json!({"url": s.url("/cut")}), &ctx()).await;
    assert!(out.content.contains("Access denied"), "{}", out.content);
    assert!(!out.content.contains("secret") && !out.content.contains("xxxx"), "{}", out.content);
}

#[tokio::test]
async fn reading_on_from_start_uses_the_copy_of_the_last_read_and_a_read_from_the_start_fetches_again() {
    let s = MockServer::start_async().await;
    let m = s
        .mock_async(|when, then| {
            when.method(GET).path("/long.txt");
            then.status(200).header("content-type", "text/plain").body("0123456789".repeat(10));
        })
        .await;
    let t = WebFetch::new(&WebConfig::default());
    let url = s.url("/long.txt");
    let out = t.run(json!({"url": url, "max_chars": 30}), &ctx()).await;
    assert!(!out.content.contains("copy fetched"), "{}", out.content);
    let out = t.run(json!({"url": url, "start": 30, "max_chars": 30}), &ctx()).await;
    assert!(out.content.contains("(read from the copy fetched "), "{}", out.content);
    assert!(
        out.content.contains("\n\n012345678901234567890123456789\n\n… 40 more characters; call again with start=60"),
        "{}",
        out.content
    );
    let out = t.run(json!({"url": url, "start": 60, "max_chars": 30}), &ctx()).await;
    assert!(out.content.contains("… 10 more characters; call again with start=90"), "{}", out.content);
    assert_eq!(m.calls_async().await, 1);
    let out = t.run(json!({"url": url, "max_chars": 30}), &ctx()).await;
    assert!(!out.content.contains("copy fetched"), "{}", out.content);
    assert_eq!(m.calls_async().await, 2);
}

#[tokio::test]
async fn a_continuation_without_a_copy_fetches_and_keeps_a_copy_for_the_next_one() {
    let s = MockServer::start_async().await;
    let m = s
        .mock_async(|when, then| {
            when.method(GET).path("/long.txt");
            then.status(200).header("content-type", "text/plain").body("0123456789".repeat(10));
        })
        .await;
    let t = WebFetch::new(&WebConfig::default());
    let url = s.url("/long.txt");
    t.run(json!({"url": url, "start": 20, "max_chars": 30}), &ctx()).await;
    t.run(json!({"url": url, "start": 50, "max_chars": 30}), &ctx()).await;
    assert_eq!(m.calls_async().await, 1);
}

#[tokio::test]
async fn the_page_copies_hold_four_pages_and_each_for_a_minute() {
    let t = WebFetch::new(&WebConfig::default());
    let keep = |key: &str, at: Instant| {
        t.keep(PageCopy {
            key: key.into(),
            at,
            final_url: Url::parse("https://example.com/").unwrap(),
            text: key.into(),
            cut: None,
            charset: None,
        })
    };
    for key in ["a", "b", "c", "d", "e"] {
        keep(key, Instant::now());
    }
    assert!(t.copy_of("a").is_none(), "the oldest copy goes when a fifth page comes");
    assert_eq!(t.copy_of("e").unwrap().text, "e");
    keep("b", Instant::now());
    assert_eq!(t.copies.lock().unwrap().len(), 4, "a page read again replaces its copy");
    keep("old", Instant::now() - Duration::from_secs(61));
    assert!(t.copy_of("old").is_none(), "a copy older than a minute is not used");
}

#[tokio::test]
async fn a_body_cut_after_a_few_bytes_says_bytes_not_zero_kb() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = sock.read(&mut buf).await;
        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 100000\r\n\r\n";
        sock.write_all(head.as_bytes()).await.unwrap();
        sock.write_all(b"just a few bytes").await.unwrap();
    });
    let t = WebFetch::new(&WebConfig::default());
    let out = t.run(json!({"url": format!("http://{addr}/few.txt")}), &ctx()).await;
    assert!(out.content.contains("connection ended after 16 bytes"), "{}", out.content);
    assert!(!out.content.contains("0 KB"), "{}", out.content);
}

#[tokio::test]
async fn a_body_cut_after_a_few_bytes_says_bytes_not_zero_kb() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = sock.read(&mut buf).await;
        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 100000\r\n\r\n";
        sock.write_all(head.as_bytes()).await.unwrap();
        sock.write_all(b"just a few bytes").await.unwrap();
    });
    let t = WebFetch::new(&WebConfig::default());
    let out = t.run(json!({"url": format!("http://{addr}/few.txt")}), &ctx()).await;
    assert!(out.content.contains("connection ended after 16 bytes"), "{}", out.content);
    assert!(!out.content.contains("0 KB"), "{}", out.content);
}
