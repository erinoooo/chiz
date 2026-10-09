//! Control-channel wire format (spec 4): u32-LE length + UTF-8 JSON frames,
//! parsed once into typed messages. The protocol is unchanged; this module
//! only gives it types.

use std::net::IpAddr;
use std::time::Duration;

use chiz_core::MAX_FRAME;
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::util::b64;

/// A connection must reach `welcome` within this long of the TLS accept.
pub const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);
/// Either side treats this long without any incoming frame as dead.
pub const DEAD_AFTER: Duration = Duration::from_secs(5);
/// Max simultaneous unauthenticated connections (spec 12).
pub const MAX_UNAUTH: usize = 4;
/// A blocked write longer than this means the peer stopped reading.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

// ------------------------------------------------------------------ frames

/// Encode one message. Our own messages are always far below the limit,
/// so an oversize frame here is a programming error, reported not sent.
pub fn encode(v: &Value) -> Vec<u8> {
    let payload = serde_json::to_vec(v).unwrap_or_default();
    if payload.len() > MAX_FRAME {
        eprintln!("BUG: refusing to send a {} byte frame", payload.len());
        return Vec::new();
    }
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload);
    out
}

#[derive(Debug, PartialEq)]
pub enum FrameError {
    Incomplete,
    Oversize,
    BadJson,
}

/// Decode one frame from the front of `buf`: (message, bytes consumed).
pub fn decode(buf: &[u8]) -> Result<(Value, usize), FrameError> {
    let Some(len) = buf.get(..4) else {
        return Err(FrameError::Incomplete);
    };
    let n = u32::from_le_bytes([len[0], len[1], len[2], len[3]]) as usize;
    if n > MAX_FRAME {
        return Err(FrameError::Oversize);
    }
    let Some(body) = buf.get(4..4 + n) else {
        return Err(FrameError::Incomplete);
    };
    let v = serde_json::from_slice(body).map_err(|_| FrameError::BadJson)?;
    Ok((v, 4 + n))
}

/// Buffered frame reader. `next` is cancel-safe: the only await is the
/// socket read, and the buffer changes only after that read completes, so
/// it can sit in a `select!` arm without ever losing bytes.
pub struct FrameReader<R> {
    inner: R,
    buf: Vec<u8>,
}

#[derive(Debug)]
pub enum ReadError {
    Io(std::io::Error),
    Oversize,
    BadJson,
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Io(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                f.write_str("the tablet dropped the connection")
            }
            ReadError::Io(e) => write!(f, "read error: {e}"),
            ReadError::Oversize => f.write_str("oversize frame"),
            ReadError::BadJson => f.write_str("malformed frame"),
        }
    }
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            buf: Vec::new(),
        }
    }

    /// Next message, `Ok(None)` on clean EOF.
    pub async fn next(&mut self) -> Result<Option<Value>, ReadError> {
        loop {
            match decode(&self.buf) {
                Ok((v, used)) => {
                    self.buf.drain(..used);
                    return Ok(Some(v));
                }
                Err(FrameError::Incomplete) => {}
                Err(FrameError::Oversize) => return Err(ReadError::Oversize),
                Err(FrameError::BadJson) => return Err(ReadError::BadJson),
            }
            let mut tmp = [0u8; 8192];
            let n = self.inner.read(&mut tmp).await.map_err(ReadError::Io)?;
            if n == 0 {
                return Ok(None);
            }
            self.buf.extend_from_slice(&tmp[..n]);
        }
    }
}

// ------------------------------------------------------------------ tablet -> PC

#[derive(Debug, PartialEq)]
pub enum PairProof {
    Qr { token: String },
    Pin { proof: String },
}

#[derive(Debug, PartialEq)]
pub enum ClientMsg {
    Ping {
        id: Value,
    },
    Hello {
        device_id: String,
        auth: String,
        pen_area: Option<(u32, u32)>,
    },
    PairRequest {
        device_id: String,
        device_name: String,
        proof: Option<PairProof>,
    },
    Button {
        id: String,
        down: bool,
    },
    Surface {
        w: u32,
        h: u32,
    },
    Bye,
    /// Unknown type or unusable fields: ignored per spec 4.
    Ignored,
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn dims(v: &Value) -> Option<(u32, u32)> {
    let w = v.get("w")?.as_u64()?.clamp(1, 1 << 16) as u32;
    let h = v.get("h")?.as_u64()?.clamp(1, 1 << 16) as u32;
    Some((w, h))
}

pub fn parse(v: &Value) -> ClientMsg {
    match v.get("t").and_then(Value::as_str).unwrap_or("") {
        "ping" => ClientMsg::Ping {
            id: v.get("id").cloned().unwrap_or(Value::from(0)),
        },
        "hello" => ClientMsg::Hello {
            device_id: s(v, "device_id"),
            auth: s(v, "auth"),
            pen_area: v.get("pen_area").and_then(dims),
        },
        "pair_request" => ClientMsg::PairRequest {
            device_id: s(v, "device_id"),
            device_name: Some(s(v, "device_name"))
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "tablet".into()),
            proof: match s(v, "mode").as_str() {
                "qr" => Some(PairProof::Qr {
                    token: s(v, "token"),
                }),
                "pin" => Some(PairProof::Pin {
                    proof: s(v, "proof"),
                }),
                _ => None,
            },
        },
        "button" => match s(v, "phase").as_str() {
            p @ ("down" | "up") => ClientMsg::Button {
                id: s(v, "id"),
                down: p == "down",
            },
            _ => ClientMsg::Ignored,
        },
        "surface" => match dims(v) {
            Some((w, h)) => ClientMsg::Surface { w, h },
            None => ClientMsg::Ignored,
        },
        "bye" => ClientMsg::Bye,
        _ => ClientMsg::Ignored,
    }
}

// ------------------------------------------------------------------ PC -> tablet

pub fn challenge(nonce: &[u8; 16], pc_name: &str, pairing_open: bool) -> Value {
    json!({
        "t": "challenge", "proto": 1, "nonce": b64(nonce),
        "pc_name": pc_name, "os": std::env::consts::OS,
        "pairing_open": pairing_open,
    })
}

pub fn welcome(session_id: u32, pen_port: u16, salt: &[u8; 16]) -> Value {
    json!({
        "t": "welcome", "session_id": session_id, "pen_port": pen_port,
        "pen_key_salt": b64(salt), "features": {"video": false},
    })
}

pub fn pong(id: Value) -> Value {
    json!({"t": "pong", "id": id})
}

/// The sender closes the connection after an `error`.
pub fn error(code: &str, message: &str) -> Value {
    json!({"t": "error", "code": code, "message": message})
}

pub fn pair_ok(secret: &[u8; 32]) -> Value {
    json!({"t": "pair_ok", "device_secret": b64(secret)})
}

pub fn pair_fail(code: &str, attempts_left: u32) -> Value {
    json!({"t": "pair_fail", "code": code, "attempts_left": attempts_left})
}

pub fn button_state(id: &str, on: bool) -> Value {
    json!({"t": "button_state", "id": id, "on": on})
}

/// By default refuse non-private sources: RFC 1918, link-local, loopback,
/// IPv6 unique-local/link-local (spec 12 requirement 7).
pub fn is_private_source(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_private() || v.is_link_local() || v.is_loopback(),
        IpAddr::V6(v) => v.is_loopback() || v.is_unique_local() || v.is_unicast_link_local(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn frame_roundtrip_and_limits() {
        let bytes = encode(&json!({"t": "ping", "id": 7, "zzz": 1}));
        let (v, used) = decode(&bytes).unwrap();
        assert_eq!((used, v["id"].as_i64()), (bytes.len(), Some(7)));
        assert_eq!(decode(&bytes[..5]), Err(FrameError::Incomplete));
        let big = (MAX_FRAME as u32 + 1).to_le_bytes();
        assert_eq!(decode(&big), Err(FrameError::Oversize));
        let mut junk = 3u32.to_le_bytes().to_vec();
        junk.extend_from_slice(b"{x}");
        assert_eq!(decode(&junk), Err(FrameError::BadJson));
    }

    /// Several frames in one segment, then one split across two writes:
    /// all decode, in order, and EOF is reported cleanly.
    #[tokio::test]
    async fn reader_handles_batched_and_split_frames() {
        let (mut client, server) = tokio::io::duplex(65536);
        let mut r = FrameReader::new(server);
        let mut batch = vec![];
        for i in 0..3 {
            batch.extend(encode(&json!({"t": "ping", "id": i})));
        }
        let split = encode(&json!({"t": "ping", "id": 3}));
        let writer = tokio::spawn(async move {
            client.write_all(&batch).await.unwrap();
            client.write_all(&split[..6]).await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            client.write_all(&split[6..]).await.unwrap();
        });
        let mut ids = vec![];
        while let Some(v) = r.next().await.unwrap() {
            ids.push(v["id"].as_i64().unwrap());
        }
        writer.await.unwrap();
        assert_eq!(ids, vec![0, 1, 2, 3]);
    }

    #[test]
    fn typed_parsing() {
        assert_eq!(parse(&json!({"t": "bye"})), ClientMsg::Bye);
        assert_eq!(parse(&json!({"t": "who"})), ClientMsg::Ignored);
        assert_eq!(
            parse(&json!({"t": "button", "id": "undo", "phase": "down"})),
            ClientMsg::Button {
                id: "undo".into(),
                down: true
            }
        );
        assert_eq!(
            parse(&json!({"t": "button", "id": "undo", "phase": "wiggle"})),
            ClientMsg::Ignored
        );
        assert_eq!(
            parse(&json!({"t": "surface", "w": 0, "h": 900})),
            ClientMsg::Surface { w: 1, h: 900 }
        );
        assert_eq!(
            parse(&json!({"t": "pair_request", "device_id": "d", "mode": "qr", "token": "x"})),
            ClientMsg::PairRequest {
                device_id: "d".into(),
                device_name: "tablet".into(),
                proof: Some(PairProof::Qr { token: "x".into() }),
            }
        );
    }

    #[test]
    fn private_source_gate() {
        for ok in [
            "192.168.1.5",
            "10.0.0.2",
            "172.20.1.1",
            "127.0.0.1",
            "169.254.1.1",
        ] {
            assert!(is_private_source(&ok.parse().unwrap()), "{ok}");
        }
        assert!(!is_private_source(&"8.8.8.8".parse().unwrap()));
        assert!(!is_private_source(&"172.32.0.1".parse().unwrap()));
    }
}
