//! The agent's client against a stand-in host over an in-memory stream.

use nexo_relay::client::{Client, Error};
use nexo_relay::frame::{read_json, write_json, MAX_SOCKET};
use nexo_relay::{proto, token};
use serde_json::{json, Value};
use tokio::io::{duplex, DuplexStream};

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A host that follows the handshake with `host_token` and then runs `body` on the stream.
fn fake_host<F, Fut>(host_token: &'static str, body: F) -> Box<DuplexStream>
where
    F: FnOnce(DuplexStream) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    let (agent, mut host) = duplex(1 << 20);
    tokio::spawn(async move {
        let nonce = proto::nonce();
        write_json(&mut host, &json!({"t": "challenge", "v": 1, "nonce": nonce}), 4096).await.unwrap();
        let hello = read_json(&mut host, 65536).await.unwrap().unwrap();
        let theirs = hello["nonce"].as_str().unwrap().to_string();
        let expect = proto::proof("agent", host_token, &nonce, &theirs);
        if !token::matches(&expect, hello["proof"].as_str().unwrap()) {
            write_json(&mut host, &json!({"t": "denied", "why": "the token did not match"}), 4096).await.unwrap();
            return;
        }
        let welcome = json!({
            "t": "welcome", "others": ["other agent"],
            "browser": {"id": "b-1", "name": "Chrome", "os": "Linux", "version": "157", "ext": "0.2.0"},
            "proof": proto::proof("host", host_token, &nonce, &theirs),
        });
        write_json(&mut host, &welcome, 65536).await.unwrap();
        body(host).await;
    });
    Box::new(agent)
}

#[tokio::test]
async fn the_handshake_gives_the_browser_and_the_other_agents() {
    let s = fake_host(TOKEN, |h| async move {
        let _keep = h;
        std::future::pending::<()>().await
    });
    let c = Client::over(s, TOKEN, "c1", "nexo test").await.unwrap();
    assert_eq!(c.info().id, "b-1");
    assert_eq!(c.info().name, "Chrome");
    assert_eq!(c.others(), ["other agent"]);
    assert!(c.is_alive());
}

#[tokio::test]
async fn a_wrong_token_is_denied_and_says_so() {
    let s = fake_host(TOKEN, |_h| async {});
    let wrong = "f".repeat(64);
    let err = Client::over(s, &wrong, "c1", "x").await.err().unwrap();
    assert!(matches!(err, Error::Denied(ref w) if w.contains("token")), "{err}");
}

#[tokio::test]
async fn a_host_that_does_not_know_the_token_is_not_trusted() {
    // The host holds another token, so its proof is wrong even though it plays the protocol.
    let (agent, mut host) = duplex(1 << 20);
    tokio::spawn(async move {
        let nonce = proto::nonce();
        write_json(&mut host, &json!({"t": "challenge", "v": 1, "nonce": nonce}), 4096).await.unwrap();
        let hello = read_json(&mut host, 65536).await.unwrap().unwrap();
        let welcome = json!({
            "t": "welcome", "browser": {"id": "b-1"},
            "proof": proto::proof("host", "another token entirely", &nonce, hello["nonce"].as_str().unwrap()),
        });
        write_json(&mut host, &welcome, 65536).await.unwrap();
    });
    let err = Client::over(Box::new(agent), TOKEN, "c1", "x").await.err().unwrap();
    assert!(matches!(err, Error::Denied(ref w) if w.contains("did not prove")), "{err}");
}

#[tokio::test]
async fn requests_get_their_own_answers_and_events_arrive_on_the_stream() {
    let s = fake_host(TOKEN, |mut h| async move {
        while let Some(msg) = read_json(&mut h, MAX_SOCKET).await.unwrap() {
            let id = msg["id"].clone();
            match msg["m"].as_str() {
                Some("tabs.list") => {
                    write_json(
                        &mut h,
                        &json!({"t": "ev", "tab": 4, "m": "Page.loadEventFired", "p": {"timestamp": 1}}),
                        MAX_SOCKET,
                    )
                    .await
                    .unwrap();
                    write_json(&mut h, &json!({"t": "res", "id": id, "r": {"tabs": []}}), MAX_SOCKET).await.unwrap();
                }
                Some("cdp") => write_json(
                    &mut h,
                    &json!({"t": "res", "id": id, "e": "that tab is not one of this session's tabs"}),
                    MAX_SOCKET,
                )
                .await
                .unwrap(),
                _ => {}
            }
        }
    });
    let c = Client::over(s, TOKEN, "c1", "x").await.unwrap();
    let mut events = c.take_events().unwrap();
    assert!(c.take_events().is_none(), "the stream can be taken once");
    let t = std::time::Duration::from_secs(5);
    assert_eq!(c.request("tabs.list", json!({}), t).await.unwrap(), json!({"tabs": []}));
    let ev = events.recv().await.unwrap();
    assert_eq!((ev.tab, ev.method.as_str()), (4, "Page.loadEventFired"));
    let err = c.request("cdp", json!({"tab": 9}), t).await.unwrap_err();
    assert!(matches!(err, Error::Remote(ref w) if w.contains("not one of")), "{err}");
}

#[tokio::test]
async fn consent_waits_for_the_answer_and_a_no_is_an_error() {
    for (answer, ok) in [(json!({"ok": true}), true), (json!({"ok": false, "why": "denied in the browser"}), false)] {
        let s = fake_host(TOKEN, move |mut h| async move {
            let msg = read_json(&mut h, MAX_SOCKET).await.unwrap().unwrap();
            assert_eq!(msg["t"], "connect");
            let mut reply = json!({"t": "consent", "id": msg["id"]});
            reply.as_object_mut().unwrap().extend(answer.as_object().unwrap().clone());
            write_json(&mut h, &reply, MAX_SOCKET).await.unwrap();
        });
        let c = Client::over(s, TOKEN, "c1", "x").await.unwrap();
        let r = c.consent().await;
        assert_eq!(r.is_ok(), ok, "{r:?}");
        if !ok {
            assert!(matches!(r, Err(Error::Denied(ref w)) if w.contains("denied in the browser")));
        }
    }
}

#[tokio::test]
async fn when_the_browser_goes_away_waiting_requests_fail_and_the_client_knows() {
    let s = fake_host(TOKEN, |mut h| async move {
        let _ = read_json(&mut h, MAX_SOCKET).await;
        write_json(&mut h, &json!({"t": "gone", "why": "the browser closed its connection to nexo"}), MAX_SOCKET)
            .await
            .unwrap();
    });
    let c = Client::over(s, TOKEN, "c1", "x").await.unwrap();
    let err = c.request("tabs.list", Value::Null, std::time::Duration::from_secs(5)).await.unwrap_err();
    assert!(matches!(err, Error::Closed(ref w) if w.contains("closed its connection")), "{err}");
    assert!(!c.is_alive());
    assert!(matches!(
        c.request("tabs.list", Value::Null, std::time::Duration::from_secs(1)).await,
        Err(Error::Closed(_))
    ));
}

#[tokio::test]
async fn a_request_nobody_answers_times_out_and_does_not_leak() {
    let s = fake_host(TOKEN, |h| async move {
        let _keep = h;
        std::future::pending::<()>().await
    });
    let c = Client::over(s, TOKEN, "c1", "x").await.unwrap();
    let err = c.request("tabs.list", Value::Null, std::time::Duration::from_millis(100)).await.unwrap_err();
    assert!(matches!(err, Error::Timeout(_)), "{err}");
}
