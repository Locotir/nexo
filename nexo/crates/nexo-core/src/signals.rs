//! The "please stop" request of a process that has no terminal to restore: headless runs, the supervisor and the
//! update screen. Ctrl+C was the only one they heard, so `kill <pid>`, a CI cancel or a service stop ended them with
//! the default action: no final result line, no queued audit lines written, no cleanup. The TUI has its own, wider
//! wait (it also takes the hangup of a closed window) and does not use this.

use std::io;

/// Resolves on the first request to stop: Ctrl+C or SIGTERM on unix, Ctrl+C, a closed console, logoff or shutdown on
/// Windows, Ctrl+C elsewhere. The handlers are installed when the future is first polled, so a request that comes
/// before that still has the default effect.
pub async fn stop() -> io::Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        // Without either handler there is nothing to wait on but Ctrl+C, which the old code waited on alone.
        if let (Ok(mut int), Ok(mut term)) = (signal(SignalKind::interrupt()), signal(SignalKind::terminate())) {
            tokio::select! { _ = int.recv() => {}, _ = term.recv() => {} }
            return Ok(());
        }
    }
    #[cfg(windows)]
    {
        use tokio::signal::windows::{ctrl_close, ctrl_logoff, ctrl_shutdown};
        if let (Ok(mut close), Ok(mut shutdown), Ok(mut logoff)) = (ctrl_close(), ctrl_shutdown(), ctrl_logoff()) {
            tokio::select! {
                r = tokio::signal::ctrl_c() => return r,
                _ = close.recv() => {},
                _ = shutdown.recv() => {},
                _ = logoff.recv() => {},
            }
            return Ok(());
        }
    }
    tokio::signal::ctrl_c().await
}

#[cfg(all(test, unix))]
mod tests {
    #[tokio::test]
    async fn a_terminate_signal_ends_the_wait_like_ctrl_c() {
        // the handler is installed by the first poll, so the signal is raised only after it: the test process lives on
        let mut wait = Box::pin(super::stop());
        assert!(poll_once(&mut wait).await.is_none(), "nothing asked to stop yet");
        // SAFETY: raising a signal in the own process; the handler above is in place
        assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);
        let ended = tokio::time::timeout(std::time::Duration::from_secs(10), wait).await;
        assert!(matches!(ended, Ok(Ok(()))), "SIGTERM did not end the wait");
    }

    /// Polls `fut` once; `None` when it is still pending.
    async fn poll_once<F: std::future::Future + Unpin>(fut: &mut F) -> Option<F::Output> {
        use std::{future::Future, pin::Pin, task::Poll};
        std::future::poll_fn(|cx| match Pin::new(&mut *fut).poll(cx) {
            Poll::Ready(v) => Poll::Ready(Some(v)),
            Poll::Pending => Poll::Ready(None),
        })
        .await
    }
}
