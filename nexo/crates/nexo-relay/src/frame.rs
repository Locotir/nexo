//! Length-prefixed JSON frames, the framing of Chrome's native messaging: a 4-byte length in the machine's byte
//! order, then that many bytes of UTF-8 JSON. The agent and the host speak the same framing on their local
//! socket, so one reader and one writer serve both links.

use serde_json::Value;
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Chrome refuses a message from a native host above 1 MiB.
pub const MAX_TO_CHROME: usize = 1024 * 1024;
/// The largest frame the host accepts from the extension. The extension splits anything bigger into chunks.
pub const MAX_FROM_CHROME: usize = 4 * 1024 * 1024;
/// The largest frame on the agent's socket: a whole screenshot or page read, already reassembled.
pub const MAX_SOCKET: usize = 64 * 1024 * 1024;

/// Reads one frame of at most `max` bytes. `None` when the stream ended cleanly before a frame started (the other
/// end closed); a frame cut short, or longer than `max`, is an error.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R, max: usize) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < len.len() {
        match r.read(&mut len[got..]).await? {
            0 if got == 0 => return Ok(None),
            0 => return Err(io::ErrorKind::UnexpectedEof.into()),
            n => got += n,
        }
    }
    let len = u32::from_ne_bytes(len) as usize;
    if len > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a frame of {len} bytes is over the {max} limit"),
        ));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    Ok(Some(body))
}

/// Writes one frame and flushes it.
pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, body: &[u8]) -> io::Result<()> {
    let len = u32::try_from(body.len()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame too large"))?;
    // One write, so two frames from two tasks never interleave their halves on a shared stream.
    let mut out = Vec::with_capacity(body.len() + 4);
    out.extend_from_slice(&len.to_ne_bytes());
    out.extend_from_slice(body);
    w.write_all(&out).await?;
    w.flush().await
}

/// Reads one frame and parses it as JSON.
pub async fn read_json<R: AsyncRead + Unpin>(r: &mut R, max: usize) -> io::Result<Option<Value>> {
    match read_frame(r, max).await? {
        None => Ok(None),
        Some(body) => {
            serde_json::from_slice(&body).map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        }
    }
}

/// Writes a JSON value as one frame, refusing one over `max` (Chrome's 1 MiB for the host's output).
pub async fn write_json<W: AsyncWrite + Unpin>(w: &mut W, v: &Value, max: usize) -> io::Result<()> {
    let body = serde_json::to_vec(v)?;
    if body.len() > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("message of {} bytes is over the {max} limit", body.len()),
        ));
    }
    write_frame(w, &body).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn a_frame_is_a_native_endian_length_then_the_json() {
        let mut buf = Vec::new();
        write_json(&mut buf, &json!({"a": 1}), MAX_TO_CHROME).await.unwrap();
        let body = br#"{"a":1}"#;
        assert_eq!(&buf[..4], &(body.len() as u32).to_ne_bytes());
        assert_eq!(&buf[4..], body);
        let back = read_json(&mut &buf[..], 100).await.unwrap().unwrap();
        assert_eq!(back, json!({"a": 1}));
    }

    #[tokio::test]
    async fn two_frames_in_a_row_and_then_a_clean_end() {
        let mut buf = Vec::new();
        write_json(&mut buf, &json!(1), 100).await.unwrap();
        write_json(&mut buf, &json!("two"), 100).await.unwrap();
        let mut r = &buf[..];
        assert_eq!(read_json(&mut r, 100).await.unwrap(), Some(json!(1)));
        assert_eq!(read_json(&mut r, 100).await.unwrap(), Some(json!("two")));
        assert_eq!(read_json(&mut r, 100).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_frame_longer_than_the_limit_is_refused_before_it_is_read() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &[b' '; 50]).await.unwrap();
        let err = read_frame(&mut &buf[..], 10).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        // Only the length was consumed: nothing of the body was allocated for.
        let mut r = &buf[..];
        let _ = read_frame(&mut r, 10).await;
        assert_eq!(r.len(), 50);
    }

    #[tokio::test]
    async fn a_frame_cut_short_is_an_error_not_a_clean_end() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"{\"abc\":1}").await.unwrap();
        for cut in [2usize, 6] {
            let err = read_frame(&mut &buf[..cut], 100).await.unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof, "cut at {cut}");
        }
    }

    #[tokio::test]
    async fn json_over_the_limit_is_not_written_and_garbage_is_not_json() {
        let mut buf = Vec::new();
        let big = json!({"x": "y".repeat(100)});
        assert_eq!(write_json(&mut buf, &big, 50).await.unwrap_err().kind(), io::ErrorKind::InvalidInput);
        assert!(buf.is_empty());
        write_frame(&mut buf, b"not json").await.unwrap();
        assert_eq!(read_json(&mut &buf[..], 100).await.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }
}
