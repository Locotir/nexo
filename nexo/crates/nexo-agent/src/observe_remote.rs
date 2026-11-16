//! Read-only, ephemeral observation transport. The contents can contain secrets.
//! Unix only; callers must keep the returned guard alive for the host lifetime.
#[cfg(unix)]
mod unix {
    use nexo_core::agent::{Observation, ObservationSnapshot};
    use std::{
        io,
        os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt},
        path::{Path, PathBuf},
        time::Duration,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{UnixListener, UnixStream},
        task::{JoinHandle, JoinSet},
    };

    /// Runtime socket directory shared by the host and observer.
    pub fn default_root() -> PathBuf {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute() && p.is_dir())
            .map(|p| p.join("nexo-observe"))
            .unwrap_or_else(|| nexo_core::paths::data_dir().join("observe"))
    }
    fn uid() -> u32 {
        unsafe { libc::geteuid() }
    }
    fn path(root: &Path, id: &str) -> io::Result<PathBuf> {
        if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid session id"));
        }
        let p = root.join(format!("{id}.sock"));
        if p.as_os_str().len() > 100 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "socket path too long"));
        }
        Ok(p)
    }
    fn private(root: &Path) -> io::Result<()> {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(root)?;
        let m = std::fs::symlink_metadata(root)?;
        if !m.is_dir() || m.uid() != uid() || m.mode() & 0o077 != 0 {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "observation directory is not private"));
        }
        Ok(())
    }
    struct Bound {
        listener: UnixListener,
        path: PathBuf,
        inode: u64,
        device: u64,
    }
    impl Bound {
        fn new(root: &Path, id: &str) -> io::Result<Self> {
            private(root)?;
            let path = path(root, id)?;
            // Never unlink an existing name: it may belong to a live host or a concurrent startup.
            let listener = UnixListener::bind(&path)?;
            let m = std::fs::symlink_metadata(&path)?;
            if !m.file_type().is_socket() || m.uid() != uid() {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "invalid socket"));
            }
            // Install cleanup before fallible permission setup; never unlink a different name.
            let bound = Self { listener, path, inode: m.ino(), device: m.dev() };
            std::fs::set_permissions(&bound.path, std::fs::Permissions::from_mode(0o600))?;
            Ok(bound)
        }
    }
    impl Drop for Bound {
        fn drop(&mut self) {
            if std::fs::symlink_metadata(&self.path).is_ok_and(|m| {
                m.file_type().is_socket() && m.dev() == self.device && m.ino() == self.inode && m.uid() == uid()
            }) {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
    // Maximum wire size, including JSON escaping and envelope overhead.
    const MAX_WIRE: usize = 4 * 1024 * 1024;
    const MAX_CLIENTS: usize = 16;

    /// Stops the endpoint and removes its socket on drop. No remote commands are accepted.
    pub struct Host(JoinHandle<()>, Option<JoinHandle<()>>);
    impl Drop for Host {
        fn drop(&mut self) {
            self.0.abort();
            if let Some(task) = &self.1 {
                task.abort();
            }
        }
    }
    /// Serve the current session, switching endpoints on identity changes. Existing socket names are never replaced.
    pub fn serve(root: PathBuf, observation: Observation) -> Host {
        serve_with_errors(root, observation, |message| tracing::warn!("{message}"))
    }
    /// Reports persistent endpoint errors once per distinct error, and retries binding.
    pub fn serve_with_errors(
        root: PathBuf,
        observation: Observation,
        report: impl Fn(String) + Send + 'static,
    ) -> Host {
        let report = std::sync::Arc::new(std::sync::Mutex::new(report));
        let incremental = tokio::spawn(serve_incremental(root.clone(), observation.clone(), report.clone()));
        Host(
            tokio::spawn(async move {
                let mut bound: Option<(String, u64, Bound)> = None;
                let mut clients = JoinSet::new();
                let mut last_error: Option<String> = None;
                loop {
                    let snap = observation.snapshot();
                    let identity = snap.session_id.as_deref();
                    if bound.as_ref().is_some_and(|(id, generation, _)| {
                        Some(id.as_str()) != identity || *generation != snap.generation
                    }) {
                        bound = None;
                        clients.abort_all();
                    }
                    if bound.is_none() {
                        if let Some(id) = identity {
                            match Bound::new(&root, id) {
                                Ok(b) => {
                                    last_error = None;
                                    bound = Some((id.to_owned(), snap.generation, b));
                                }
                                Err(e) => {
                                    let hint = if e.kind() == io::ErrorKind::AddrInUse {
                                        " An existing socket is never removed automatically; if no session is using it, remove it manually."
                                    } else {
                                        ""
                                    };
                                    let message = format!("Observation unavailable for session {id}: {e}.{hint}");
                                    if last_error.as_deref() != Some(&message) {
                                        (report.lock().unwrap_or_else(|e| e.into_inner()))(message.clone());
                                        last_error = Some(message);
                                    }
                                }
                            }
                        }
                    }
                    if let Some((id, generation, b)) = &bound {
                        tokio::select! {
                            accepted = b.listener.accept() => {
                                match accepted {
                                    Ok((mut stream, _)) if stream.peer_cred().is_ok_and(|c| c.uid() == uid()) => {
                                        if clients.len() < MAX_CLIENTS {
                                            let s = observation.snapshot();
                                            if s.session_id.as_deref() == Some(id) && s.generation == *generation {
                                                clients.spawn(async move {
                                                    if let Ok(data) = serde_json::to_vec(&s) {
                                                        if data.len() <= MAX_WIRE {
                                                            let _ = tokio::time::timeout(Duration::from_millis(500), async {
                                                                stream.write_all(&(data.len() as u32).to_be_bytes()).await?;
                                                                stream.write_all(&data).await
                                                            }).await;
                                                        }
                                                    }
                                                });
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        let message = format!("Observation accept failed: {e}");
                                        if last_error.as_deref() != Some(&message) {
                                            (report.lock().unwrap_or_else(|e| e.into_inner()))(message.clone());
                                            last_error = Some(message);
                                        }
                                        tokio::time::sleep(Duration::from_millis(100)).await;
                                    }
                                    _ => {}
                                }
                            }
                            Some(_) = clients.join_next(), if !clients.is_empty() => {}
                            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                        }
                    } else {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }),
            Some(incremental),
        )
    }
    /// Versioned bounded polling. Each caller retains its own cursor; a resync is explicit.
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub enum ObservationUpdate {
        Initial(ObservationSnapshot),
        Delta(ObservationSnapshot),
        Resync(ObservationSnapshot),
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    struct ObserveRequest {
        version: u8,
        cursor: Option<ObservationSnapshot>,
    }

    async fn serve_incremental(
        root: PathBuf,
        observation: Observation,
        report: std::sync::Arc<std::sync::Mutex<impl Fn(String) + Send + 'static>>,
    ) {
        let root = root.join("v1");
        let mut bound: Option<(String, u64, Bound)> = None;
        let mut clients = JoinSet::new();
        let mut last_bind_error = None;
        let mut last_accept_error = None;
        let mut last_client_error = None;
        loop {
            let snap = observation.snapshot();
            if bound.as_ref().is_some_and(|(id, generation, _)| {
                snap.session_id.as_deref() != Some(id) || snap.generation != *generation
            }) {
                bound = None;
                clients.abort_all();
            }
            if bound.is_none() {
                if let Some(id) = snap.session_id.as_deref() {
                    match Bound::new(&root, id) {
                        Ok(b) => {
                            last_bind_error = None;
                            bound = Some((id.to_owned(), snap.generation, b));
                        }
                        Err(e) => {
                            let hint = if e.kind() == io::ErrorKind::AddrInUse {
                                "; an existing socket is never removed automatically; if unused, remove it manually"
                            } else {
                                ""
                            };
                            let message = format!("Incremental observation bind failed: {:?}{hint}", e.kind());
                            if last_bind_error.as_ref() != Some(&message) {
                                (report.lock().unwrap_or_else(|e| e.into_inner()))(message.clone());
                                last_bind_error = Some(message);
                            }
                        }
                    }
                }
            }
            if let Some((id, generation, b)) = &bound {
                tokio::select! {
                    accepted = b.listener.accept() => {
                        match accepted {
                          Ok((mut stream, _)) => {
                            last_accept_error = None;
                            if stream.peer_cred().is_ok_and(|c| c.uid() == uid()) && clients.len() < MAX_CLIENTS {
                                let observation = observation.clone();
                                let id = id.clone();
                                let generation = *generation;
                                clients.spawn(async move {
                                    tokio::time::timeout(Duration::from_secs(2), async {
                                        let n = stream.read_u32().await? as usize;
                                        if n > 4096 { return Err(io::Error::new(io::ErrorKind::InvalidData, "request too large")); }
                                        let mut bytes = vec![0; n];
                                        stream.read_exact(&mut bytes).await?;
                                        let req: ObserveRequest = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                                        if req.version != 1 { return Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported version")); }
                                        let update = match req.cursor {
                                            None => ObservationUpdate::Initial(observation.snapshot()),
                                            Some(cursor) => match observation.snapshot_since(&cursor) {
                                                Ok(s) => ObservationUpdate::Delta(s),
                                                Err(s) => ObservationUpdate::Resync(s),
                                            },
                                        };
                                        let s = match &update { ObservationUpdate::Initial(s) | ObservationUpdate::Delta(s) | ObservationUpdate::Resync(s) => s };
                                        // Never expose the new session through an old socket during rebinding.
                                        if s.session_id.as_deref() != Some(&id) || s.generation != generation { return Err(io::Error::new(io::ErrorKind::InvalidData, "identity changed")); }
                                        let data = serde_json::to_vec(&update).map_err(io::Error::other)?;
                                        if data.len() > MAX_WIRE { return Err(io::Error::new(io::ErrorKind::InvalidData, "response too large")); }
                                        stream.write_u32(data.len() as u32).await?;
                                        stream.write_all(&data).await
                                    }).await.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "client timed out"))?
                                });
                            }
                          }
                          Err(e) => {
                            let message = format!("Incremental observation accept failed: {:?}", e.kind());
                            if last_accept_error.as_ref() != Some(&message) {
                                (report.lock().unwrap_or_else(|e| e.into_inner()))(message.clone());
                                last_accept_error = Some(message);
                            }
                            tokio::time::sleep(Duration::from_millis(100)).await;
                          }
                        }
                    }
                    Some(result) = clients.join_next(), if !clients.is_empty() => {
                        let message = match result {
                            Ok(Ok(())) => { last_client_error = None; None }
                            Ok(Err(e)) => Some(format!("Incremental observation client failed: {:?}", e.kind())),
                            Err(e) if e.is_cancelled() => None,
                            Err(_) => Some("Incremental observation client task failed".to_owned()),
                        };
                        if let Some(message) = message {
                            if last_client_error.as_ref() != Some(&message) {
                                (report.lock().unwrap_or_else(|e| e.into_inner()))(message.clone());
                                last_client_error = Some(message);
                            }
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                }
            } else {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }

    /// Poll the versioned endpoint. After initial observation, callers must pin instance and
    /// session identity: a resync with a different instance must not be followed silently.
    pub async fn snapshot_since(
        root: &Path,
        id: &str,
        cursor: Option<&ObservationSnapshot>,
    ) -> io::Result<ObservationUpdate> {
        let root = root.join("v1");
        let path = path(&root, id)?;
        private(&root)?;
        let m = std::fs::symlink_metadata(&path)?;
        if !m.file_type().is_socket() || m.uid() != uid() || m.mode() & 0o077 != 0 {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "observation socket is not private"));
        }
        // Only cursor metadata is needed, not its potentially large events.
        let wire_cursor = cursor.map(|s| {
            let mut s = s.clone();
            s.events.clear();
            s
        });
        let request =
            serde_json::to_vec(&ObserveRequest { version: 1, cursor: wire_cursor }).map_err(io::Error::other)?;
        if request.len() > 4096 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "cursor too large"));
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            let mut stream = UnixStream::connect(path).await?;
            if !stream.peer_cred().is_ok_and(|c| c.uid() == uid()) {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "wrong socket owner"));
            }
            stream.write_u32(request.len() as u32).await?;
            stream.write_all(&request).await?;
            let n = stream.read_u32().await? as usize;
            if n > MAX_WIRE {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "response too large"));
            }
            let mut bytes = vec![0; n];
            stream.read_exact(&mut bytes).await?;
            let update: ObservationUpdate = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            let s = match &update {
                ObservationUpdate::Initial(s) | ObservationUpdate::Delta(s) | ObservationUpdate::Resync(s) => s,
            };
            if s.session_id.as_deref() != Some(id) {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "session identity changed"));
            }
            match (&update, cursor) {
                (ObservationUpdate::Initial(_), None) => {}
                (ObservationUpdate::Resync(_), Some(_)) => {}
                (ObservationUpdate::Delta(next), Some(previous)) => {
                    if next.instance_id != previous.instance_id
                        || next.generation != previous.generation
                        || next.session_id != previous.session_id
                        || next.sequence < previous.sequence
                        || next.dropped > previous.sequence
                        || next.events.len() as u64 != next.sequence - previous.sequence
                        || next.events.iter().enumerate().any(|(i, e)| e.sequence != previous.sequence + i as u64 + 1)
                    {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid observation delta"));
                    }
                }
                _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "unexpected observation response")),
            }
            Ok(update)
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "observation timed out"))?
    }

    /// Result of one intervention. Unknown means the request may have been consumed;
    /// never retry automatically. Accepted means incorporated in memory, not persisted or answered.
    #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    pub enum InterventionResult {
        Accepted,
        Rejected(String),
        Unknown,
    }

    #[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct InterventionRequest {
        id: String,
        target: nexo_core::agent::ObservationTarget,
        text: String,
    }
    const MAX_INTERVENTION: usize = 64 * 1024;
    const MAX_REQUESTS: usize = 4096;
    const MAX_PENDING: usize = 64;
    type Receipts = std::sync::Arc<
        tokio::sync::Mutex<
            std::collections::HashMap<uuid::Uuid, ([u8; 32], tokio::sync::watch::Receiver<Option<InterventionResult>>)>,
        >,
    >;

    /// Separate, same-user-only control socket. The receipt table is retained for the host
    /// lifetime and fails closed at capacity rather than evicting deduplication keys.
    pub fn serve_interventions(root: PathBuf, observation: Observation, commands: nexo_core::agent::CommandTx) -> Host {
        serve_interventions_with_errors(root, observation, commands, |message| tracing::warn!("{message}"))
    }

    /// Report sanitized control endpoint errors, suppressing repeats until recovery.
    pub fn serve_interventions_with_errors(
        root: PathBuf,
        observation: Observation,
        commands: nexo_core::agent::CommandTx,
        report: impl Fn(String) + Send + 'static,
    ) -> Host {
        Host(
            tokio::spawn(async move {
                let root = root.join("control");
                let receipts: Receipts = Default::default();
                let mut bound: Option<(String, Bound)> = None;
                let mut clients = JoinSet::new();
                let mut last_bind_error = None;
                let mut last_accept_error = None;
                loop {
                    let snap = observation.snapshot();
                    if bound.as_ref().is_some_and(|(id, _)| snap.session_id.as_deref() != Some(id)) {
                        bound = None;
                    }
                    if bound.is_none() {
                        if let Some(id) = snap.session_id.as_deref() {
                            match Bound::new(&root, id) {
                                Ok(b) => {
                                    last_bind_error = None;
                                    last_accept_error = None;
                                    bound = Some((id.to_owned(), b));
                                }
                                Err(e) => {
                                    let hint = if e.kind() == io::ErrorKind::AddrInUse {
                                        "; an existing socket is never removed automatically; if unused, remove it manually"
                                    } else {
                                        ""
                                    };
                                    let message = format!("Intervention bind failed: {:?}{hint}", e.kind());
                                    if last_bind_error.as_ref() != Some(&message) {
                                        report(message.clone());
                                        last_bind_error = Some(message);
                                    }
                                }
                            }
                        }
                    }
                    if let Some((_, b)) = &bound {
                        tokio::select! {
                            accepted = b.listener.accept() => {
                                match accepted {
                                    Ok((stream, _)) => {
                                        last_accept_error = None;
                                        if stream.peer_cred().is_ok_and(|c| c.uid() == uid()) && clients.len() < MAX_CLIENTS {
                                            clients.spawn(intervention_connection(stream, receipts.clone(), commands.clone()));
                                        }
                                    }
                                    Err(e) => {
                                        let message = format!("Intervention accept failed: {:?}", e.kind());
                                        if last_accept_error.as_ref() != Some(&message) {
                                            report(message.clone());
                                            last_accept_error = Some(message);
                                        }
                                        tokio::time::sleep(Duration::from_millis(100)).await;
                                    }
                                }
                            }
                            Some(_) = clients.join_next(), if !clients.is_empty() => {}
                            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                        }
                    } else {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }),
            None,
        )
    }

    async fn intervention_connection(
        mut stream: UnixStream,
        receipts: Receipts,
        commands: nexo_core::agent::CommandTx,
    ) {
        use nexo_core::agent::{AgentCommand, DirectedReply, DirectedResult};
        let read = tokio::time::timeout(Duration::from_secs(2), async {
            let n = stream.read_u32().await? as usize;
            if n > MAX_INTERVENTION {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "request too large"));
            }
            let mut bytes = vec![0; n];
            stream.read_exact(&mut bytes).await?;
            serde_json::from_slice::<InterventionRequest>(&bytes).map_err(io::Error::other)
        })
        .await;
        let Ok(Ok(req)) = read else {
            return;
        };
        let Ok(id) = uuid::Uuid::parse_str(&req.id) else {
            return;
        };
        if req.text.trim().is_empty() {
            return;
        }
        // Length-prefix each field so different field boundaries cannot collide. The ID is
        // already the canonical table key and is intentionally not included in the digest.
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        for field in [req.target.instance_id.as_bytes(), req.target.session_id.as_bytes(), req.text.as_bytes()] {
            hash.update((field.len() as u64).to_be_bytes());
            hash.update(field);
        }
        hash.update(req.target.generation.to_be_bytes());
        let digest: [u8; 32] = hash.finalize().into();
        let mut table = receipts.lock().await;
        let mut rx = if let Some((old, rx)) = table.get(&id) {
            if old != &digest {
                drop(table);
                let _ = write_intervention_result(
                    &mut stream,
                    &InterventionResult::Rejected("request id already used".into()),
                )
                .await;
                return;
            }
            rx.clone()
        } else {
            if table.len() >= MAX_REQUESTS {
                drop(table);
                let _ = write_intervention_result(
                    &mut stream,
                    &InterventionResult::Rejected("receipt capacity reached".into()),
                )
                .await;
                return;
            }
            if table.values().filter(|(_, rx)| rx.borrow().is_none()).count() >= MAX_PENDING {
                drop(table);
                let _ = write_intervention_result(
                    &mut stream,
                    &InterventionResult::Rejected("pending capacity reached".into()),
                )
                .await;
                return;
            }
            let (tx, rx) = tokio::sync::watch::channel(None);
            let (reply, result) = tokio::sync::oneshot::channel();
            table.insert(id, (digest, rx.clone()));
            let command =
                AgentCommand::SubmitDirected { target: req.target, text: req.text, reply: DirectedReply::new(reply) };
            let sent = commands.send(command).is_ok();
            // This task must outlive the socket client, and retains the receipt even on disconnect.
            tokio::spawn(async move {
                let outcome = if !sent {
                    InterventionResult::Rejected("agent unavailable".into())
                } else {
                    match result.await {
                        Ok(DirectedResult::Accepted) => InterventionResult::Accepted,
                        Ok(DirectedResult::Rejected(reason)) => InterventionResult::Rejected(reason.into()),
                        Err(_) => InterventionResult::Unknown,
                    }
                };
                let _ = tx.send(Some(outcome));
            });
            rx
        };
        drop(table);
        let outcome = tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                if let Some(result) = rx.borrow_and_update().clone() {
                    break result;
                }
                if rx.changed().await.is_err() {
                    break InterventionResult::Unknown;
                }
            }
        })
        .await
        .unwrap_or(InterventionResult::Unknown);
        let _ = write_intervention_result(&mut stream, &outcome).await;
    }
    async fn write_intervention_result(stream: &mut UnixStream, result: &InterventionResult) -> io::Result<()> {
        let bytes = serde_json::to_vec(result).map_err(io::Error::other)?;
        tokio::time::timeout(Duration::from_secs(2), async {
            stream.write_u32(bytes.len() as u32).await?;
            stream.write_all(&bytes).await
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "receipt timed out"))?
    }

    /// Submit exactly one attempt. `id` must be a fresh UUID per logical request. An error is
    /// definitely unsent; Unknown means bytes may have reached the host. No automatic retry.
    pub async fn intervene(
        root: &Path,
        target: nexo_core::agent::ObservationTarget,
        id: String,
        text: String,
    ) -> io::Result<InterventionResult> {
        if id.len() != 36 || uuid::Uuid::parse_str(&id).is_err() || text.trim().is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid request"));
        }
        let root = root.join("control");
        let path = path(&root, &target.session_id)?;
        private(&root)?;
        let m = std::fs::symlink_metadata(&path)?;
        if !m.file_type().is_socket() || m.uid() != uid() || m.mode() & 0o077 != 0 {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "control socket is not private"));
        }
        let bytes = serde_json::to_vec(&InterventionRequest { id, target, text }).map_err(io::Error::other)?;
        if bytes.len() > MAX_INTERVENTION {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "request too large"));
        }
        let mut stream = tokio::time::timeout(Duration::from_secs(2), UnixStream::connect(path))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "connect timed out"))??;
        if !stream.peer_cred().is_ok_and(|c| c.uid() == uid()) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "wrong socket owner"));
        }
        let response = tokio::time::timeout(Duration::from_secs(5), async {
            stream.write_u32(bytes.len() as u32).await?;
            stream.write_all(&bytes).await?;
            let n = stream.read_u32().await? as usize;
            if n > 4096 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "receipt too large"));
            }
            let mut reply = vec![0; n];
            stream.read_exact(&mut reply).await?;
            serde_json::from_slice::<InterventionResult>(&reply).map_err(io::Error::other)
        })
        .await;
        Ok(match response {
            Ok(Ok(result)) => result,
            _ => InterventionResult::Unknown,
        })
    }
    /// One bounded atomic snapshot. Poll to follow sequence; generation changes or dropped events require resync.
    pub async fn snapshot(root: &Path, id: &str) -> io::Result<ObservationSnapshot> {
        let path = path(root, id)?;
        private(root)?;
        let m = std::fs::symlink_metadata(&path)?;
        if !m.file_type().is_socket() || m.uid() != uid() || m.mode() & 0o077 != 0 {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "observation socket is not private"));
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            let mut stream = UnixStream::connect(path).await?;
            if !stream.peer_cred().is_ok_and(|c| c.uid() == uid()) {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "wrong socket owner"));
            }
            let n = stream.read_u32().await? as usize;
            if n > MAX_WIRE {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot too large"));
            }
            let mut bytes = vec![0; n];
            stream.read_exact(&mut bytes).await?;
            let s: ObservationSnapshot = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            if s.session_id.as_deref() != Some(id) {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "session identity changed"));
            }
            Ok(s)
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "observation timed out"))?
    }
}
#[cfg(unix)]
pub use unix::*;

#[cfg(all(test, unix))]
mod tests;
