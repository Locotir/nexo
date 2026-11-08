//! The binary frames a front end sends: a ten-byte header, a payload and a one-byte checksum.
//!
//! ```text
//! offset 0   4 bytes  magic "VLT1"
//! offset 4   1 byte   kind (1 ping, 2 put, 3 get, 4 delete)
//! offset 5   1 byte   flags (unused, kept for later)
//! offset 6   4 bytes  payload length, big endian
//! offset 10  n bytes  payload
//! offset 10+n 1 byte  checksum: the payload bytes added up modulo 256
//! ```

use crate::error::{Result, VaultError};

/// The first four bytes of every frame.
pub const MAGIC: [u8; 4] = *b"VLT1";

/// The bytes before the payload.
pub const HEADER_LEN: usize = 10;

/// The largest payload a frame may carry.
pub const MAX_PAYLOAD: usize = 1 << 20;

/// What a frame asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ping,
    Put,
    Get,
    Delete,
}

impl Kind {
    fn from_byte(b: u8) -> Result<Kind> {
        match b {
            1 => Ok(Kind::Ping),
            2 => Ok(Kind::Put),
            3 => Ok(Kind::Get),
            4 => Ok(Kind::Delete),
            other => Err(VaultError::Malformed(format!("unknown frame kind {other}"))),
        }
    }

    fn byte(self) -> u8 {
        match self {
            Kind::Ping => 1,
            Kind::Put => 2,
            Kind::Get => 3,
            Kind::Delete => 4,
        }
    }
}

/// One decoded frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub kind: Kind,
    pub flags: u8,
    pub payload: Vec<u8>,
}

/// The checksum of a payload.
pub fn checksum(payload: &[u8]) -> u8 {
    payload.iter().fold(0u8, |acc, b| acc.wrapping_add(*b))
}

impl Frame {
    /// A frame with no flags.
    pub fn new(kind: Kind, payload: Vec<u8>) -> Frame {
        Frame { kind, flags: 0, payload }
    }

    /// The bytes of the frame.
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.payload.len() > MAX_PAYLOAD {
            return Err(VaultError::Invalid(format!("a payload may have at most {MAX_PAYLOAD} bytes")));
        }
        let mut out = Vec::with_capacity(HEADER_LEN + self.payload.len() + 1);
        out.extend_from_slice(&MAGIC);
        out.push(self.kind.byte());
        out.push(self.flags);
        out.extend_from_slice(&(self.payload.len() as u32).to_be_bytes());
        out.extend_from_slice(&self.payload);
        out.push(checksum(&self.payload));
        Ok(out)
    }

    /// Reads the first frame in `buf` and says how many bytes it took.
    pub fn decode(buf: &[u8]) -> Result<(Frame, usize)> {
        if buf.len() < HEADER_LEN {
            return Err(VaultError::Malformed("the frame is shorter than its header".into()));
        }
        if buf[..4] != MAGIC {
            return Err(VaultError::Malformed("bad magic".into()));
        }
        let kind = Kind::from_byte(buf[4])?;
        let flags = buf[5];
        let len = u32::from_be_bytes([buf[6], buf[7], buf[8], buf[9]]) as usize;
        let end = HEADER_LEN + len;
        let payload = buf[HEADER_LEN..end].to_vec();
        if buf[end] != checksum(&payload) {
            return Err(VaultError::Malformed("the checksum does not match".into()));
        }
        Ok((Frame { kind, flags, payload }, end + 1))
    }

    /// Every frame in `buf`, which must hold nothing else.
    pub fn decode_all(buf: &[u8]) -> Result<Vec<Frame>> {
        let mut frames = Vec::new();
        let mut at = 0;
        while at < buf.len() {
            let (frame, used) = Frame::decode(&buf[at..])?;
            frames.push(frame);
            at += used;
        }
        Ok(frames)
    }

    /// The text command this frame stands for, as `Handler::handle` reads it.
    pub fn to_line(&self) -> Result<String> {
        match self.kind {
            Kind::Ping => Ok("WHOAMI".to_string()),
            Kind::Put => {
                let (title, body) = parse_put(&self.payload)?;
                if title.contains(['|', '\n', '\r']) {
                    return Err(VaultError::Invalid("a title may not hold | or line breaks".into()));
                }
                Ok(format!("CREATE {title}|{body}|"))
            }
            Kind::Get | Kind::Delete => {
                let id = parse_id(&self.payload)?;
                let verb = if self.kind == Kind::Get { "GET" } else { "DELETE" };
                Ok(format!("{verb} {id}"))
            }
        }
    }
}

/// The payload of a put frame: a two-byte title length, the title, then the body.
pub fn encode_put(title: &str, body: &str) -> Result<Vec<u8>> {
    let len = u16::try_from(title.len()).map_err(|_| VaultError::Invalid("the title is too long".into()))?;
    let mut out = len.to_be_bytes().to_vec();
    out.extend_from_slice(title.as_bytes());
    out.extend_from_slice(body.as_bytes());
    Ok(out)
}

/// The title and body in a put payload.
pub fn parse_put(payload: &[u8]) -> Result<(String, String)> {
    let head = payload.get(..2).ok_or_else(|| VaultError::Malformed("the put payload has no title length".into()))?;
    let title_len = usize::from(u16::from_be_bytes([head[0], head[1]]));
    let title = payload
        .get(2..2 + title_len)
        .ok_or_else(|| VaultError::Malformed("the title is longer than the payload".into()))?;
    let body = &payload[2 + title_len..];
    let text =
        |b: &[u8]| String::from_utf8(b.to_vec()).map_err(|_| VaultError::Invalid("the text is not UTF-8".into()));
    Ok((text(title)?, text(body)?))
}

/// The document id in a get or delete payload: eight bytes, big endian.
pub fn parse_id(payload: &[u8]) -> Result<u64> {
    let bytes: [u8; 8] = payload
        .try_into()
        .map_err(|_| VaultError::Malformed(format!("an id payload has 8 bytes, not {}", payload.len())))?;
    Ok(u64::from_be_bytes(bytes))
}
