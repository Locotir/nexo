use super::*;
use nexo_core::agent::{channels, AgentEvent};
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

#[tokio::test]
async fn incremental_fanout_loss_and_reset() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("run");
    let (handle, ends) = channels();
    let _host = serve(root.clone(), handle.observation.clone());
    ends.events.send(AgentEvent::SessionChanged { session_id: "same".into(), title: String::new() }).unwrap();
    let start = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Ok(ObservationUpdate::Initial(s)) = snapshot_since(&root, "same", None).await {
                break s;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    for _ in 0..3 {
        ends.events.send(AgentEvent::TextDelta { text: "x".into() }).unwrap();
    }
    let a = match snapshot_since(&root, "same", Some(&start)).await.unwrap() {
        ObservationUpdate::Delta(s) => s,
        _ => panic!(),
    };
    let b = match snapshot_since(&root, "same", Some(&start)).await.unwrap() {
        ObservationUpdate::Delta(s) => s,
        _ => panic!(),
    };
    assert_eq!(a, b);
    assert_eq!(a.events.len(), 3);
    assert!(
        matches!(snapshot_since(&root, "same", Some(&a)).await.unwrap(), ObservationUpdate::Delta(s) if s.events.is_empty())
    );
    for _ in 0..513 {
        ends.events.send(AgentEvent::TextDelta { text: "x".into() }).unwrap();
    }
    assert!(
        matches!(snapshot_since(&root, "same", Some(&start)).await.unwrap(), ObservationUpdate::Resync(s) if s.dropped > 0)
    );
    ends.events.send(AgentEvent::HistoryCleared).unwrap();
    ends.events.send(AgentEvent::SessionChanged { session_id: "same".into(), title: String::new() }).unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        matches!(snapshot_since(&root, "same", Some(&a)).await.unwrap(), ObservationUpdate::Resync(s) if s.generation != a.generation)
    );
}

#[tokio::test]
async fn intervention_receipts_deduplicate_and_keep_target() {
    use nexo_core::agent::{AgentCommand, DirectedResult, ObservationTarget};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("run");
    let (handle, mut ends) = channels();
    let _host = serve_interventions(root.clone(), handle.observation.clone(), handle.commands.clone());
    ends.events.send(AgentEvent::SessionChanged { session_id: "target".into(), title: String::new() }).unwrap();
    let snap = handle.observation.snapshot();
    let target =
        ObservationTarget { instance_id: snap.instance_id, session_id: "target".into(), generation: snap.generation };
    tokio::time::timeout(Duration::from_secs(3), async {
        while !root.join("control/target.sock").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let a = tokio::spawn({
        let root = root.clone();
        let target = target.clone();
        let id = id.clone();
        async move { intervene(&root, target, id, "hello".into()).await.unwrap() }
    });
    let command = ends.commands.recv().await.unwrap();
    match command {
        AgentCommand::SubmitDirected { target: got, text, reply } => {
            assert_eq!(got, target);
            assert_eq!(text, "hello");
            reply.resolve(DirectedResult::Accepted);
        }
        _ => panic!("unexpected command"),
    }
    assert_eq!(a.await.unwrap(), InterventionResult::Accepted);
    assert_eq!(
        intervene(&root, target.clone(), id.to_uppercase(), "hello".into()).await.unwrap(),
        InterventionResult::Accepted
    );
    assert!(matches!(
        intervene(&root, target.clone(), id, "different".into()).await.unwrap(),
        InterventionResult::Rejected(_)
    ));
    assert!(ends.commands.try_recv().is_err());
    let b = tokio::spawn({
        let root = root.clone();
        let target = target.clone();
        async move { intervene(&root, target, uuid::Uuid::new_v4().to_string(), "stale".into()).await.unwrap() }
    });
    if let AgentCommand::SubmitDirected { reply, .. } = ends.commands.recv().await.unwrap() {
        reply.resolve(DirectedResult::Rejected("observation changed"));
    } else {
        panic!("unexpected command");
    }
    assert_eq!(b.await.unwrap(), InterventionResult::Rejected("observation changed".into()));
}

#[tokio::test]
async fn snapshot_identity_reset_permissions_and_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("run");
    let (handle, ends) = channels();
    let host = serve(root.clone(), handle.observation.clone());
    ends.events.send(AgentEvent::SessionChanged { session_id: "first".into(), title: String::new() }).unwrap();
    ends.events.send(AgentEvent::TextDelta { text: "hello".into() }).unwrap();
    let s = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Ok(s) = snapshot(&root, "first").await {
                break s;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(s.sequence, 1);
    assert_eq!(std::fs::metadata(&root).unwrap().permissions().mode() & 0o777, 0o700);
    assert_eq!(std::fs::metadata(root.join("first.sock")).unwrap().permissions().mode() & 0o777, 0o600);
    assert!(snapshot(&root, "../first").await.is_err());
    ends.events.send(AgentEvent::HistoryCleared).unwrap();
    ends.events.send(AgentEvent::SessionChanged { session_id: "second".into(), title: String::new() }).unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(snapshot(&root, "first").await.is_err());
    let s2 = snapshot(&root, "second").await.unwrap();
    assert!(s2.generation > s.generation);
    assert!(s2.events.is_empty());
    drop(host);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!root.join("second.sock").exists());
}
#[tokio::test]
async fn escaped_snapshot_and_slow_client() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("run");
    let (handle, ends) = channels();
    let _host = serve(root.clone(), handle.observation.clone());
    ends.events.send(AgentEvent::SessionChanged { session_id: "escaped".into(), title: String::new() }).unwrap();
    for _ in 0..512 {
        ends.events.send(AgentEvent::TextDelta { text: "\\".repeat(500) }).unwrap();
    }
    let expected = handle.observation.snapshot();
    assert!(serde_json::to_vec(&expected).unwrap().len() > 1024 * 1024);
    let s = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Ok(s) = snapshot(&root, "escaped").await {
                break s;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(s, expected);
    let _slow = tokio::net::UnixStream::connect(root.join("escaped.sock")).await.unwrap();
    let s = tokio::time::timeout(Duration::from_millis(400), snapshot(&root, "escaped")).await.unwrap().unwrap();
    assert_eq!(s, expected);
}

#[tokio::test]
async fn observation_socket_does_not_forward_commands_or_permission_decisions() {
    use tokio::io::AsyncWriteExt;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("run");
    let (handle, mut ends) = channels();
    let _host = serve(root.clone(), handle.observation.clone());
    ends.events.send(AgentEvent::SessionChanged { session_id: "target".into(), title: String::new() }).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if snapshot(&root, "target").await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();

    for payload in [
        r#"{"cmd":"submit","text":"should not arrive","attachments":[]}"#,
        r#"{"cmd":"permission","id":"request","decision":"allow"}"#,
    ] {
        let mut stream = tokio::net::UnixStream::connect(root.join("target.sock")).await.unwrap();
        // The observation endpoint has no request protocol and never forwards input.
        let _ = stream.write_all(payload.as_bytes()).await;
        drop(stream);
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(matches!(ends.commands.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)));
}

#[tokio::test]
async fn replacement_socket_is_not_removed_on_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("run");
    let (handle, ends) = channels();
    let host = serve(root.clone(), handle.observation.clone());
    ends.events.send(AgentEvent::SessionChanged { session_id: "replace".into(), title: String::new() }).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while snapshot(&root, "replace").await.is_err() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let path = root.join("replace.sock");
    std::fs::remove_file(&path).unwrap();
    let replacement = std::os::unix::net::UnixListener::bind(&path).unwrap();
    drop(host);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(std::os::unix::net::UnixStream::connect(&path).is_ok());
    drop(replacement);
}

#[tokio::test]
async fn snapshot_rejects_wrong_session_and_oversize_response() {
    use tokio::io::AsyncWriteExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("run");
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.join("target.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (handle, ends) = channels();
    ends.events.send(AgentEvent::SessionChanged { session_id: "other".into(), title: String::new() }).unwrap();
    let bytes = serde_json::to_vec(&handle.observation.snapshot()).unwrap();
    let server = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        s.write_u32(bytes.len() as u32).await.unwrap();
        s.write_all(&bytes).await.unwrap();
        let (mut s, _) = listener.accept().await.unwrap();
        s.write_u32(4 * 1024 * 1024 + 1).await.unwrap();
    });
    assert_eq!(snapshot(&root, "target").await.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(snapshot(&root, "target").await.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    server.await.unwrap();
    let nonexistent = dir.path().join("not-created");
    assert_eq!(snapshot(&nonexistent, "../bad").await.unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
    assert!(!nonexistent.exists());
}

#[tokio::test]
async fn incremental_errors_are_reported_and_binding_recovers() {
    use tokio::io::AsyncWriteExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("run");
    std::fs::create_dir_all(root.join("v1")).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::set_permissions(root.join("v1"), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.join("v1/busy.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let (handle, ends) = channels();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let _host = serve_with_errors(root.clone(), handle.observation.clone(), move |msg| {
        let _ = tx.send(msg);
    });
    ends.events.send(AgentEvent::SessionChanged { session_id: "busy".into(), title: String::new() }).unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await.unwrap().unwrap();
    assert!(msg.contains("Incremental observation bind failed"));
    assert!(msg.contains("remove it manually"));
    assert!(tokio::time::timeout(Duration::from_millis(250), rx.recv()).await.is_err());
    assert!(path.exists());
    drop(listener);
    std::fs::remove_file(&path).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while snapshot_since(&root, "busy", None).await.is_err() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let mut stream = tokio::net::UnixStream::connect(&path).await.unwrap();
    let private = b"private request contents";
    stream.write_u32(private.len() as u32).await.unwrap();
    stream.write_all(private).await.unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await.unwrap().unwrap();
    assert!(msg.contains("Incremental observation client failed"));
    assert!(!msg.contains("private"));
}

#[tokio::test]
async fn intervention_bind_reports_once_and_recovers() {
    let dir = tempfile::tempdir().unwrap();
    // short: a socket path holds 100 bytes at most here, and a macOS temp folder takes 60 of them already
    let root = dir.path().join("private");
    std::fs::create_dir_all(root.join("control")).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::set_permissions(root.join("control"), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.join("control/private-session.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let (handle, ends) = channels();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let _host = serve_interventions_with_errors(
        root.clone(),
        handle.observation.clone(),
        handle.commands.clone(),
        move |msg| {
            let _ = tx.send(msg);
        },
    );
    ends.events
        .send(AgentEvent::SessionChanged { session_id: "private-session".into(), title: String::new() })
        .unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await.unwrap().unwrap();
    assert!(msg.contains("Intervention bind failed: AddrInUse"));
    assert!(msg.contains("remove it manually"));
    assert!(!msg.contains("private"));
    assert!(tokio::time::timeout(Duration::from_millis(350), rx.recv()).await.is_err());
    assert!(path.exists());
    drop(listener);
    std::fs::remove_file(&path).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if tokio::net::UnixStream::connect(&path).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let second = root.join("control/second.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&second).unwrap();
    ends.events.send(AgentEvent::SessionChanged { session_id: "second".into(), title: String::new() }).unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await.unwrap().unwrap();
    assert!(msg.contains("Intervention bind failed: AddrInUse"));
}

#[tokio::test]
async fn occupied_socket_reports_without_removing_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("run");
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.join("busy.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let (handle, ends) = channels();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let _host = serve_with_errors(root.clone(), handle.observation.clone(), move |msg| {
        let _ = tx.send(msg);
    });
    ends.events.send(AgentEvent::SessionChanged { session_id: "busy".into(), title: String::new() }).unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await.unwrap().unwrap();
    assert!(msg.contains("remove it manually"));
    assert!(path.exists());
    assert!(std::os::unix::net::UnixStream::connect(&path).is_ok());
    assert!(tokio::time::timeout(Duration::from_millis(250), rx.recv()).await.is_err());
    drop(listener);
    std::fs::remove_file(&path).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if snapshot(&root, "busy").await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
}
