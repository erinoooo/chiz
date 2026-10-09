//! The single owner of connection/session state (spec 4, 12).
//!
//! One tablet session at a time. Everything that belongs to it (pen key,
//! UDP receiver, pen-area aspect, the outbox of its connection task) lives
//! in one `Active` value, so it is created and destroyed as a unit: nothing
//! can outlive its session or leak into the next one.
//!
//! `Hub` is plain synchronous logic behind one std mutex in `App`; callers
//! never hold it across an await. Connection tasks talk to it through the
//! small methods below and receive frames for their socket on their outbox.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::time::Instant;

use chiz_core::{derive_pen_key, PenReceiver};
use tokio::sync::mpsc;

use crate::pairing::PairingState;
use crate::store::{PairedStore, PairedTablet};
use crate::util::{ct_eq, os_random};
use crate::wire::{self, PairProof, MAX_UNAUTH};

/// Work for a connection task, sent by anyone holding its outbox.
#[derive(Debug)]
pub enum Outbound {
    Frame(Vec<u8>),
    /// Close the connection; the text is logged as the reason.
    Close(String),
}

pub type Outbox = mpsc::UnboundedSender<Outbound>;

/// The live tablet session.
pub struct Active {
    pub conn: u64,
    pub sid: u32,
    pub device_id: String,
    pub device_name: String,
    pub peer: IpAddr,
    pub pen_key: [u8; 32],
    pub rx: PenReceiver,
    pub aspect: (u32, u32),
    pub since: Instant,
    out: Outbox,
}

impl Active {
    /// Queue a message for the tablet (dropped silently if the connection
    /// task is already gone; its teardown is then in flight).
    pub fn send(&self, v: &serde_json::Value) {
        let _ = self.out.send(Outbound::Frame(wire::encode(v)));
    }

    pub fn close(&self, reason: &str) {
        let _ = self.out.send(Outbound::Close(reason.to_string()));
    }

    pub fn dropped(&self) -> u64 {
        let d = &self.rx.drops;
        (d.bad_magic + d.bad_session + d.bad_tag + d.wrong_ip + d.old_seq + d.old_tms + d.identical)
            as u64
    }
}

#[derive(Debug, PartialEq)]
pub enum HelloError {
    NotPaired,
    BadAuth,
    /// Another tablet holds the single slot.
    Busy,
}

impl HelloError {
    pub fn reply(&self) -> serde_json::Value {
        match self {
            HelloError::NotPaired => wire::error("not_paired", "unknown tablet"),
            HelloError::BadAuth => wire::error("auth_failed", "bad auth"),
            HelloError::Busy => wire::error("busy", "another tablet is connected"),
        }
    }
}

pub struct Accepted {
    pub sid: u32,
    pub salt: [u8; 16],
    /// The same tablet's previous session, now evicted: the caller closes
    /// its connection and releases held input.
    pub replaced: Option<Active>,
}

#[derive(Debug, PartialEq)]
pub enum PairResult {
    Paired([u8; 32]),
    Failed {
        code: &'static str,
        attempts_left: u32,
    },
    /// Too many failures: the window just closed.
    Closed,
    NotOpen,
}

/// Snapshot for the GUI.
#[derive(Clone, Debug, Default)]
pub struct HubStatus {
    /// (device name, device id) of the connected tablet.
    pub tablet: Option<(String, String)>,
    pub dropped: u64,
    pub pairing_open: bool,
    pub pairing_left_s: u64,
    pub pin: String,
    pub paired: Vec<(String, String)>,
}

pub struct Hub {
    pub paired: PairedStore,
    pub pairing: PairingState,
    auth: AuthTracker,
    unauth: usize,
    active: Option<Active>,
    next_sid: u32,
    next_conn: u64,
}

/// Why a new connection was refused before the handshake.
#[derive(Debug, PartialEq)]
pub enum Refusal {
    PublicSource,
    Blocked,
    TooManyPending,
}

impl Hub {
    pub fn new(paired: PairedStore) -> Self {
        Self {
            paired,
            pairing: PairingState::closed(),
            auth: AuthTracker::default(),
            unauth: 0,
            active: None,
            next_sid: 0,
            next_conn: 0,
        }
    }

    /// Gate a fresh connection (private source, auth backoff, 4 pending
    /// max). On success the connection holds an unauthenticated slot until
    /// `hello` succeeds or `release_unauth` is called.
    pub fn admit(&mut self, ip: IpAddr, now: f64, allow_public: bool) -> Result<u64, Refusal> {
        if !allow_public && !wire::is_private_source(&ip) {
            return Err(Refusal::PublicSource);
        }
        if self.auth.is_blocked(&ip, now) {
            return Err(Refusal::Blocked);
        }
        if self.unauth >= MAX_UNAUTH {
            return Err(Refusal::TooManyPending);
        }
        self.unauth += 1;
        self.next_conn += 1;
        Ok(self.next_conn)
    }

    pub fn release_unauth(&mut self) {
        self.unauth = self.unauth.saturating_sub(1);
    }

    /// Authenticate `hello`. Success makes this connection the active
    /// session (evicting the same tablet's older session) and frees its
    /// unauthenticated slot.
    #[allow(clippy::too_many_arguments)]
    pub fn hello(
        &mut self,
        conn: u64,
        device_id: &str,
        auth_b64: &str,
        nonce: &[u8; 16],
        peer: IpAddr,
        pen_area: Option<(u32, u32)>,
        now: f64,
        out: Outbox,
    ) -> Result<Accepted, HelloError> {
        let Some(PairedTablet { secret, name }) = self.paired.get(device_id).cloned() else {
            self.auth.record_failure(peer, now);
            return Err(HelloError::NotPaired);
        };
        let expect = chiz_core::compute_auth(&secret, nonce, device_id);
        if !ct_eq(expect.as_bytes(), auth_b64.as_bytes()) {
            self.auth.record_failure(peer, now);
            return Err(HelloError::BadAuth);
        }
        self.auth.record_success(&peer);
        let replaced = match &self.active {
            Some(a) if a.device_id == device_id => self.active.take(),
            Some(_) => return Err(HelloError::Busy),
            None => None,
        };
        self.next_sid = self.next_sid.wrapping_add(1).max(1);
        let sid = self.next_sid;
        let mut salt = [0u8; 16];
        os_random(&mut salt);
        self.active = Some(Active {
            conn,
            sid,
            device_id: device_id.to_string(),
            device_name: name,
            peer,
            pen_key: derive_pen_key(&secret, &salt),
            rx: PenReceiver::new(sid, &peer.to_string()),
            aspect: pen_area.unwrap_or((2100, 1600)),
            since: Instant::now(),
            out,
        });
        self.release_unauth();
        Ok(Accepted {
            sid,
            salt,
            replaced,
        })
    }

    /// `pair_request` inside an open window (spec 3).
    pub fn pair(
        &mut self,
        device_id: &str,
        device_name: &str,
        proof: Option<&PairProof>,
        nonce: &[u8; 16],
        own_fp: &[u8; 32],
        now: f64,
    ) -> PairResult {
        if !self.pairing.is_open(now) {
            return PairResult::NotOpen;
        }
        let ok = !device_id.is_empty()
            && match proof {
                Some(PairProof::Qr { token }) => self.pairing.verify_qr(token),
                Some(PairProof::Pin { proof }) => {
                    self.pairing.verify_pin(proof, own_fp, nonce, device_id)
                }
                None => false,
            };
        if ok {
            let mut secret = [0u8; 32];
            os_random(&mut secret);
            self.paired.insert(
                device_id,
                PairedTablet {
                    secret,
                    name: device_name.chars().take(64).collect(),
                },
            );
            self.pairing.window.record_success();
            return PairResult::Paired(secret);
        }
        let code = match proof {
            Some(PairProof::Qr { .. }) => "bad_token",
            _ => "bad_pin",
        };
        let left = self.pairing.attempts_left().saturating_sub(1);
        if self.pairing.window.record_failure() == "closed" {
            PairResult::Closed
        } else {
            PairResult::Failed {
                code,
                attempts_left: left,
            }
        }
    }

    pub fn active(&self) -> Option<&Active> {
        self.active.as_ref()
    }

    pub fn active_mut(&mut self) -> Option<&mut Active> {
        self.active.as_mut()
    }

    /// Connection `conn` ended. Returns its session only if it is still the
    /// active one (a replaced connection ending must not end its successor).
    pub fn end(&mut self, conn: u64) -> Option<Active> {
        match &self.active {
            Some(a) if a.conn == conn => self.active.take(),
            _ => None,
        }
    }

    /// Forget a tablet: drop its secret and end its live session, if any.
    pub fn forget(&mut self, device_id: &str) -> Option<Active> {
        self.paired.remove(device_id);
        match &self.active {
            Some(a) if a.device_id == device_id => self.active.take(),
            _ => None,
        }
    }

    pub fn status(&mut self, now: f64) -> HubStatus {
        let open = self.pairing.is_open(now);
        let opened = self.pairing.window.open_at.unwrap_or(now);
        HubStatus {
            tablet: self
                .active
                .as_ref()
                .map(|a| (a.device_name.clone(), a.device_id.clone())),
            dropped: self.active.as_ref().map(Active::dropped).unwrap_or(0),
            pairing_open: open,
            pairing_left_s: if open {
                (120.0 - (now - opened)).clamp(0.0, 120.0) as u64
            } else {
                0
            },
            pin: if open {
                self.pairing.pin.clone()
            } else {
                String::new()
            },
            paired: self.paired.list(),
        }
    }
}

/// After 5 failed authentications from one IP within 60 s, that IP is
/// ignored for 60 s (spec 12).
#[derive(Default)]
struct AuthTracker {
    fails: HashMap<IpAddr, VecDeque<f64>>,
    blocked_until: HashMap<IpAddr, f64>,
}

impl AuthTracker {
    fn is_blocked(&self, ip: &IpAddr, now: f64) -> bool {
        self.blocked_until.get(ip).is_some_and(|u| now < *u)
    }

    fn record_failure(&mut self, ip: IpAddr, now: f64) {
        let q = self.fails.entry(ip).or_default();
        while q.front().is_some_and(|t| now - *t > 60.0) {
            q.pop_front();
        }
        q.push_back(now);
        if q.len() >= 5 {
            self.blocked_until.insert(ip, now + 60.0);
            self.fails.remove(&ip);
        }
    }

    fn record_success(&mut self, ip: &IpAddr) {
        self.fails.remove(ip);
        self.blocked_until.remove(ip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IP: &str = "192.168.1.9";

    fn ip() -> IpAddr {
        IP.parse().unwrap()
    }

    fn hub_with(devs: &[(&str, u8)]) -> Hub {
        let mut store = PairedStore::in_memory();
        for (id, b) in devs {
            store.insert(
                id,
                PairedTablet {
                    secret: [*b; 32],
                    name: format!("tab-{id}"),
                },
            );
        }
        Hub::new(store)
    }

    fn outbox() -> (Outbox, mpsc::UnboundedReceiver<Outbound>) {
        mpsc::unbounded_channel()
    }

    fn hello(h: &mut Hub, conn: u64, dev: &str, b: u8) -> Result<Accepted, HelloError> {
        let nonce = [3u8; 16];
        let auth = chiz_core::compute_auth(&[b; 32], &nonce, dev);
        h.hello(conn, dev, &auth, &nonce, ip(), None, 0.0, outbox().0)
    }

    #[test]
    fn single_slot_busy_and_same_device_replace() {
        let mut h = hub_with(&[("A", 1), ("B", 2)]);
        let c1 = h.admit(ip(), 0.0, false).unwrap();
        let first = hello(&mut h, c1, "A", 1).unwrap();
        assert!(first.replaced.is_none());
        let c2 = h.admit(ip(), 0.0, false).unwrap();
        assert_eq!(hello(&mut h, c2, "B", 2).err(), Some(HelloError::Busy));
        let c3 = h.admit(ip(), 0.0, false).unwrap();
        let again = hello(&mut h, c3, "A", 1).unwrap();
        // The old session is handed back for teardown, with a new sid.
        assert_eq!(again.replaced.as_ref().map(|a| a.conn), Some(c1));
        assert_ne!(again.sid, first.sid);
        // The replaced connection ending must not end its successor.
        assert!(h.end(c1).is_none());
        assert_eq!(h.active().map(|a| a.conn), Some(c3));
        assert!(h.end(c3).is_some());
        assert!(h.active().is_none());
    }

    #[test]
    fn pending_slots_are_capped_and_freed() {
        let mut h = hub_with(&[("A", 1)]);
        let conns: Vec<u64> = (0..4).map(|_| h.admit(ip(), 0.0, false).unwrap()).collect();
        assert_eq!(h.admit(ip(), 0.0, false), Err(Refusal::TooManyPending));
        hello(&mut h, conns[0], "A", 1).unwrap(); // authenticating frees a slot
        assert!(h.admit(ip(), 0.0, false).is_ok());
        assert_eq!(h.admit(ip(), 0.0, false), Err(Refusal::TooManyPending));
        h.release_unauth();
        assert!(h.admit(ip(), 0.0, false).is_ok());
    }

    #[test]
    fn failures_block_the_ip_and_public_is_refused() {
        let mut h = hub_with(&[("A", 1)]);
        for _ in 0..5 {
            let c = h.admit(ip(), 0.0, false).unwrap();
            assert_eq!(hello(&mut h, c, "A", 9).err(), Some(HelloError::BadAuth));
            h.release_unauth();
        }
        assert_eq!(h.admit(ip(), 1.0, false), Err(Refusal::Blocked));
        assert!(h.admit(ip(), 61.5, false).is_ok());
        let public: IpAddr = "8.8.8.8".parse().unwrap();
        assert_eq!(h.admit(public, 0.0, false), Err(Refusal::PublicSource));
        assert!(h.admit(public, 0.0, true).is_ok());
    }

    #[test]
    fn unknown_tablet_and_forget() {
        let mut h = hub_with(&[("A", 1)]);
        let c = h.admit(ip(), 0.0, false).unwrap();
        assert_eq!(hello(&mut h, c, "Z", 1).err(), Some(HelloError::NotPaired));
        let c = h.admit(ip(), 0.0, false).unwrap();
        hello(&mut h, c, "A", 1).unwrap();
        assert_eq!(h.forget("A").map(|a| a.conn), Some(c));
        assert!(h.paired.get("A").is_none());
        let c = h.admit(ip(), 0.0, false).unwrap();
        assert_eq!(hello(&mut h, c, "A", 1).err(), Some(HelloError::NotPaired));
    }

    #[test]
    fn pairing_window_rules() {
        let mut h = hub_with(&[]);
        let nonce = [5u8; 16];
        let fp = [6u8; 32];
        let qr = |t: &str| PairProof::Qr { token: t.into() };
        assert_eq!(
            h.pair("T", "Tab", Some(&qr("x")), &nonce, &fp, 0.0),
            PairResult::NotOpen
        );
        let (token, _pin) = h.pairing.open(100.0);
        let bad = h.pair("T", "Tab", Some(&qr("AAAA")), &nonce, &fp, 101.0);
        assert_eq!(
            bad,
            PairResult::Failed {
                code: "bad_token",
                attempts_left: 4
            }
        );
        let good = h.pair(
            "T",
            "Tab",
            Some(&qr(&crate::util::b64(&token))),
            &nonce,
            &fp,
            102.0,
        );
        let PairResult::Paired(secret) = good else {
            panic!("expected pairing to succeed: {good:?}");
        };
        assert_eq!(h.paired.get("T").map(|t| t.secret), Some(secret));
        assert!(!h.pairing.is_open(103.0)); // closes after success
                                            // Five failures close a fresh window.
        h.pairing.open(200.0);
        let pin = |p: &str| PairProof::Pin { proof: p.into() };
        for i in 0..4 {
            assert!(matches!(
                h.pair("U", "x", Some(&pin("nope")), &nonce, &fp, 201.0 + i as f64),
                PairResult::Failed { .. }
            ));
        }
        assert_eq!(
            h.pair("U", "x", Some(&pin("nope")), &nonce, &fp, 206.0),
            PairResult::Closed
        );
    }
}
