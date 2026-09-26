//! Control channel: framing, handshake, session table, auth rate limits.
//! Spec sections 3, 4, 12. Pure logic (no sockets) so it is unit-testable;
//! the tokio tasks in `main.rs` / `pen.rs` drive it.
//!
//! TLS note: production traffic runs inside TLS 1.3 (rustls + rcgen
//! self-signed ECDSA P-256, fingerprint pinning). The framing and session
//! rules here sit *above* the TLS stream, so tests exercise them over plain
//! memory buffers; `main.rs` marks the exact splice point for the acceptor.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;

use chiz_core::MAX_FRAME;

// ------------------------------------------------------------- framing

/// Encode one control message: u32 LE length + UTF-8 JSON (spec 4).
pub fn encode_frame(value: &serde_json::Value) -> Result<Vec<u8>, &'static str> {
    let payload = serde_json::to_vec(value).map_err(|_| "json encode")?;
    if payload.len() > MAX_FRAME {
        return Err("frame too large");
    }
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

#[derive(Debug, PartialEq)]
pub enum FrameError {
    Incomplete,
    Oversize,
    BadUtf8,
    BadJson,
}

/// Decode one frame from the front of `buf`. On success returns the message
/// and total bytes consumed; caller drains them. Unknown types/fields are
/// passed through untouched — receivers MUST ignore them (spec 4).
pub fn decode_frame(buf: &[u8]) -> Result<(serde_json::Value, usize), FrameError> {
    if buf.len() < 4 {
        return Err(FrameError::Incomplete);
    }
    let n = u32::from_le_bytes(buf[0..4].try_into().unwrap()) as usize;
    if n > MAX_FRAME {
        return Err(FrameError::Oversize);
    }
    if buf.len() < 4 + n {
        return Err(FrameError::Incomplete);
    }
    let text = std::str::from_utf8(&buf[4..4 + n]).map_err(|_| FrameError::BadUtf8)?;
    let v: serde_json::Value = serde_json::from_str(text).map_err(|_| FrameError::BadJson)?;
    Ok((v, 4 + n))
}

// ------------------------------------------------------------- handshake

/// Handshake deadline: companion MUST close a connection that has not
/// reached `welcome` within 10 s of TCP accept (spec 4).
pub const HANDSHAKE_DEADLINE_SECS: f64 = 10.0;
/// Liveness: either side treats 5 s without any incoming message as dead.
pub const DEAD_AFTER_SECS: f64 = 5.0;
/// Ping interval (tablet -> PC).
#[allow(dead_code)]
pub const PING_INTERVAL_SECS: f64 = 1.0;
/// Max simultaneous unauthenticated connections (spec 12).
pub const MAX_UNAUTH: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ConnStage {
    /// Connected, waiting for hello/pair_request. `since` = accept time (s).
    Unauth { since: f64 },
    /// Authenticated session for a device.
    Authed { session_id: u32 },
}

/// One tablet at a time (spec 4): a second *authenticated* tablet gets
/// `error busy`; the same `device_id` reconnecting replaces its old session.
/// Session end MUST release keys/pen (caller invokes the platform hook).
#[derive(Default)]
pub struct SessionTable {
    /// device_id -> (session_id, connection id).
    live: HashMap<String, (u32, u64)>,
    next_session_id: u32,
}

impl SessionTable {
    /// Try to authenticate `device_id` on connection `conn`. Returns the new
    /// session id, or the id of the connection that currently holds the slot
    /// (caller sends `error busy` to the newcomer).
    pub fn auth(&mut self, conn: u64, device_id: &str) -> Result<u32, u64> {
        if let Some((_, holder)) = self.live.get(device_id) {
            if *holder != conn {
                // Same device reconnecting: evict the stale entry; the old
                // socket gets closed by the caller (release_all first).
                let old = *holder;
                let sid = self.next_session_id + 1;
                self.next_session_id = sid;
                self.live.insert(device_id.into(), (sid, conn));
                let _ = old;
                return Ok(sid);
            }
            return Ok(self.live[device_id].0);
        }
        if !self.live.is_empty() {
            // A *different* device holds the single slot.
            return Err(self.live.values().next().unwrap().1);
        }
        let sid = self.next_session_id + 1;
        self.next_session_id = sid;
        self.live.insert(device_id.into(), (sid, conn));
        Ok(sid)
    }

    pub fn remove_conn(&mut self, conn: u64) {
        self.live.retain(|_, (_, c)| *c != conn);
    }

    pub fn session_of(&self, conn: u64) -> Option<u32> {
        self.live.values().find(|(_, c)| *c == conn).map(|(s, _)| *s)
    }

    #[allow(dead_code)]
    pub fn is_holder(&self, conn: u64) -> bool {
        self.live.values().any(|(_, c)| *c == conn)
    }

    /// Live tablet device ids (0 or 1 entries), for status screens.
    pub fn tablets(&self) -> Vec<String> {
        let mut v: Vec<String> = self.live.keys().cloned().collect();
        v.sort();
        v
    }

    /// Forget a tablet: drop its session slot. Returns nothing; the caller
    /// also drops pen state and queued frames (socket reaps in <= 5 s).
    pub fn remove_device(&mut self, device_id: &str) {
        self.live.remove(device_id);
    }
}

// ------------------------------------------------------------- auth rate limit (spec 12)

/// After 5 failed authentications from one IP within 60 s, that IP is
/// ignored for 60 s. Sliding window of failure timestamps per IP.
#[derive(Default)]
pub struct AuthTracker {
    fails: HashMap<IpAddr, VecDeque<f64>>,
    blocked_until: HashMap<IpAddr, f64>,
}

impl AuthTracker {
    pub fn is_blocked(&self, ip: &IpAddr, now: f64) -> bool {
        self.blocked_until.get(ip).map(|u| now < *u).unwrap_or(false)
    }

    /// Record a failure; returns true if the IP just became blocked.
    pub fn record_failure(&mut self, ip: IpAddr, now: f64) -> bool {
        let q = self.fails.entry(ip).or_default();
        while q.front().map(|t| now - *t > 60.0).unwrap_or(false) {
            q.pop_front();
        }
        q.push_back(now);
        if q.len() >= 5 {
            self.blocked_until.insert(ip, now + 60.0);
            self.fails.remove(&ip);
            return true;
        }
        false
    }

    pub fn record_success(&mut self, ip: &IpAddr) {
        self.fails.remove(ip);
        self.blocked_until.remove(ip);
    }
}

// ------------------------------------------------------------- private source check (spec 12)

/// By default refuse non-private sources: RFC 1918, link-local, IPv6
/// unique-local/link-local (spec 12 requirement 7).
pub fn is_private_source(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            o[0] == 10
                || (o[0] == 172 && (16..32).contains(&o[1]))
                || (o[0] == 192 && o[1] == 168)
                || (o[0] == 169 && o[1] == 254) // link-local
                || o[0] == 127 // loopback (tests, adb reverse later)
        }
        IpAddr::V6(v) => v.is_loopback() || v.is_unique_local() || v.is_unicast_link_local(),
    }
}

// ------------------------------------------------------------- message helpers

/// Build an `error` message. Sender closes the connection after sending.
pub fn error_msg(code: &str, message: &str) -> serde_json::Value {
    serde_json::json!({"t": "error", "code": code, "message": message})
}

/// Extract the message type; unknown types return None (caller ignores them).
pub fn msg_type(v: &serde_json::Value) -> Option<&str> {
    v.get("t").and_then(|t| t.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip_and_limits() {
        let m = serde_json::json!({"t":"ping","id":7,"zzz_unknown":1});
        let bytes = encode_frame(&m).unwrap();
        let (back, used) = decode_frame(&bytes).unwrap();
        assert_eq!(used, bytes.len());
        assert_eq!(back["t"], "ping"); // unknown field preserved, ignored by logic
        let mut big = vec![0u8; 4];
        big[0..4].copy_from_slice(&(MAX_FRAME as u32 + 1).to_le_bytes());
        assert_eq!(decode_frame(&big), Err(FrameError::Oversize));
        assert_eq!(decode_frame(&bytes[..5]), Err(FrameError::Incomplete));
    }

    #[test]
    fn session_single_tablet_with_replace() {
        let mut t = SessionTable::default();
        assert_eq!(t.auth(1, "dev-A"), Ok(1)); // first tablet in
        assert!(t.auth(2, "dev-B").is_err()); // second tablet -> busy
        assert_eq!(t.auth(3, "dev-A"), Ok(2)); // same device reconnects
        assert!(!t.is_holder(1)); // stale conn evicted
        t.remove_conn(3);
        assert_eq!(t.auth(4, "dev-B"), Ok(3)); // slot free again
    }

    #[test]
    fn auth_tracker_blocks_after_five() {
        let mut tr = AuthTracker::default();
        let ip: IpAddr = "192.168.1.9".parse().unwrap();
        for _ in 0..4 {
            assert!(!tr.record_failure(ip, 0.0));
        }
        assert!(tr.record_failure(ip, 1.0)); // 5th -> blocked
        assert!(tr.is_blocked(&ip, 30.0));
        assert!(!tr.is_blocked(&ip, 61.1));
    }

    #[test]
    fn private_source_gate() {
        assert!(is_private_source(&"192.168.1.5".parse().unwrap()));
        assert!(is_private_source(&"10.0.0.2".parse().unwrap()));
        assert!(is_private_source(&"172.20.1.1".parse().unwrap()));
        assert!(!is_private_source(&"8.8.8.8".parse().unwrap()));
        assert!(is_private_source(&"127.0.0.1".parse().unwrap()));
    }
}
