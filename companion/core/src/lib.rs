//! Chiz v1 core: protocol crypto, pen packets, mapping, pressure curve,
//! receiver rules, control validation, pairing, profiles, button actions.
//! Spec sections 3, 4, 5, 8, 11. Mirrors protocol/chiz_proto.py byte for byte.

pub mod action;
pub mod failsafe;
pub mod platform;
pub mod profile;
pub mod qr;

use aes_gcm::{aead::{Aead, KeyInit}, Aes256Gcm, Nonce};
use base64::Engine as _;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;

pub const PROTO_VERSION: u32 = 1;
pub const MAGIC: u8 = 0xC1;
pub const MAX_FRAME: usize = 65536;

/// SHA-256 of a DER-encoded certificate = the pairing fingerprint (spec 3).
pub fn sha256_fp(der: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    let mut h = Sha256::new();
    h.update(der);
    h.finalize().into()
}

pub fn derive_pen_key(device_secret: &[u8; 32], salt: &[u8; 16]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(salt), device_secret);
    let mut out = [0u8; 32];
    hk.expand(b"chiz-pen-v1", &mut out).expect("hkdf expand");
    out
}

pub fn compute_auth(device_secret: &[u8; 32], nonce: &[u8; 16], device_id: &str) -> String {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(device_secret).expect("hmac");
    mac.update(b"chiz-auth-v1");
    mac.update(nonce);
    mac.update(device_id.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

pub fn compute_pin_proof(pin: &str, fp_seen: &[u8; 32], nonce: &[u8; 16], device_id: &str) -> String {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(pin.as_bytes()).expect("hmac");
    mac.update(b"chiz-pair-v1");
    mac.update(fp_seen);
    mac.update(nonce);
    mac.update(device_id.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PenRecord {
    pub phase: u8, // 0 hover,1 down,2 move,3 up,4 leave,5 cancel
    pub eraser: bool, pub barrel1: bool, pub barrel2: bool,
    pub x: u16, pub y: u16, pub pressure: u16,
    pub tilt_x: i8, pub tilt_y: i8, pub distance: u16, pub t_ms: u32,
}

pub fn encode_record(r: &PenRecord) -> [u8; 16] {
    let mut flags = r.phase & 0x07;
    if r.eraser { flags |= 0x08; }
    if r.barrel1 { flags |= 0x10; }
    if r.barrel2 { flags |= 0x20; }
    let mut b = [0u8; 16];
    b[0] = flags; b[1] = 0;
    b[2..4].copy_from_slice(&r.x.to_le_bytes());
    b[4..6].copy_from_slice(&r.y.to_le_bytes());
    b[6..8].copy_from_slice(&r.pressure.to_le_bytes());
    b[8] = r.tilt_x as u8; b[9] = r.tilt_y as u8;
    b[10..12].copy_from_slice(&r.distance.to_le_bytes());
    b[12..16].copy_from_slice(&r.t_ms.to_le_bytes());
    b
}

pub fn decode_record(b: &[u8; 16]) -> PenRecord {
    let flags = b[0];
    PenRecord {
        phase: flags & 0x07,
        eraser: flags & 0x08 != 0,
        barrel1: flags & 0x10 != 0,
        barrel2: flags & 0x20 != 0,
        x: u16::from_le_bytes([b[2], b[3]]),
        y: u16::from_le_bytes([b[4], b[5]]),
        pressure: u16::from_le_bytes([b[6], b[7]]),
        tilt_x: b[8] as i8, tilt_y: b[9] as i8,
        distance: u16::from_le_bytes([b[10], b[11]]),
        t_ms: u32::from_le_bytes([b[12], b[13], b[14], b[15]]),
    }
}

pub fn pen_nonce(seq: u32) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[0..4].copy_from_slice(&seq.to_le_bytes());
    n
}

/// Build a full UDP datagram: header(10) + ciphertext + tag(16).
pub fn encrypt_datagram(key: &[u8; 32], session_id: u32, seq: u32, records: &[PenRecord]) -> Vec<u8> {
    assert!(records.len() <= 16);
    let mut pt = vec![records.len() as u8];
    for r in records { pt.extend_from_slice(&encode_record(r)); }
    let mut header = [0u8; 10];
    header[0] = MAGIC; header[1] = 0;
    header[2..6].copy_from_slice(&session_id.to_le_bytes());
    header[6..10].copy_from_slice(&seq.to_le_bytes());
    let cipher = Aes256Gcm::new_from_slice(key).expect("key");
    let blob = cipher.encrypt(
        Nonce::from_slice(&pen_nonce(seq)),
        aes_gcm::aead::Payload { msg: &pt, aad: &header }).expect("encrypt");
    [header.to_vec(), blob].concat()
}

/// Decrypt a datagram; returns (session_id, seq, records). Errors on bad
/// magic / short packet / failed tag. Caller checks session/ip/seq ordering.
pub fn decrypt_datagram(key: &[u8; 32], datagram: &[u8]) -> Result<(u32, u32, Vec<PenRecord>), &'static str> {
    if datagram.len() < 10 + 1 + 16 { return Err("too short"); }
    if datagram[0] != MAGIC { return Err("bad magic"); }
    let session_id = u32::from_le_bytes(datagram[2..6].try_into().unwrap());
    let seq = u32::from_le_bytes(datagram[6..10].try_into().unwrap());
    let cipher = Aes256Gcm::new_from_slice(key).expect("key");
    let pt = cipher.decrypt(
        Nonce::from_slice(&pen_nonce(seq)),
        aes_gcm::aead::Payload { msg: &datagram[10..], aad: &datagram[..10] })
        .map_err(|_| "bad tag")?;
    let count = pt[0] as usize;
    if count > 16 || pt.len() != 1 + 16 * count { return Err("bad count"); }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let mut b = [0u8; 16];
        b.copy_from_slice(&pt[1 + 16 * i..1 + 16 * (i + 1)]);
        out.push(decode_record(&b));
    }
    Ok((session_id, seq, out))
}

/// Rotate (x,y) in 0..1 clockwise per spec section 8.
pub fn rotate_uv(x: f64, y: f64, rotation: u16) -> (f64, f64) {
    match rotation {
        0 => (x, y), 90 => (1.0 - y, x),
        180 => (1.0 - x, 1.0 - y), 270 => (y, 1.0 - x),
        _ => panic!("rotation must be 0/90/180/270"),
    }
}

/// Map pen 0..1 to monitor pixels with optional keep-aspect letterboxing.
pub fn map_pen_to_px(x: f64, y: f64, pen_w: f64, pen_h: f64,
                     mon_x: f64, mon_y: f64, mon_w: f64, mon_h: f64,
                     area: (f64, f64, f64, f64), keep_aspect: bool, rotation: u16) -> (i64, i64) {
    let (mut u, mut v) = rotate_uv(x, y, rotation);
    let mut rp = pen_w / pen_h;
    if rotation == 90 || rotation == 270 { rp = pen_h / pen_w; }
    let (ax, ay, aw, ah) = area;
    let area_w = mon_w * aw; let area_h = mon_h * ah;
    let ra = area_w / area_h;
    if keep_aspect {
        if rp > ra { let fw = ra / rp; u = ((u - (1.0 - fw) / 2.0) / fw).clamp(0.0, 1.0); }
        else if rp < ra { let fh = rp / ra; v = ((v - (1.0 - fh) / 2.0) / fh).clamp(0.0, 1.0); }
    }
    ((mon_x + ax * mon_w + u * (area_w - 1.0)).round() as i64,
     (mon_y + ay * mon_h + v * (area_h - 1.0)).round() as i64)
}

fn bezier(curve: &[f64; 4], t: f64) -> (f64, f64) {
    let (x1, y1, x2, y2) = (curve[0], curve[1], curve[2], curve[3]);
    let mt = 1.0 - t;
    (3.0*mt*mt*t*x1 + 3.0*mt*t*t*x2 + t*t*t,
     3.0*mt*mt*t*y1 + 3.0*mt*t*t*y2 + t*t*t)
}

/// Cubic-Bezier pressure curve, bisection 12 iterations (spec 8).
pub fn apply_pressure_curve(curve: &[f64; 4], p: f64) -> f64 {
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..12 {
        let mid = (lo + hi) / 2.0;
        if bezier(curve, mid).0 < p { lo = mid; } else { hi = mid; }
    }
    bezier(curve, (lo + hi) / 2.0).1
}

// ---------------------------------------------------------- receiver (spec 5)

#[derive(Default, Debug)]
pub struct DropCounters {
    pub bad_magic: u32, pub bad_session: u32, pub bad_tag: u32,
    pub wrong_ip: u32, pub old_seq: u32, pub old_tms: u32, pub identical: u32,
}

pub struct PenReceiver {
    pub session_id: u32,
    pub control_peer_ip: String,
    pub highest_seq: u32,
    pub last_t_ms: Option<u32>,
    pub last_record_bytes: Option<[u8; 16]>,
    pub drops: DropCounters,
    pub last_datagram_at: Option<f64>,
    pub pen_is_down: bool,
    pub pen_in_range: bool,
}

impl PenReceiver {
    pub fn new(session_id: u32, control_peer_ip: &str) -> Self {
        Self { session_id, control_peer_ip: control_peer_ip.into(),
               highest_seq: 0, last_t_ms: None, last_record_bytes: None,
               drops: DropCounters::default(), last_datagram_at: None,
               pen_is_down: false, pen_in_range: false }
    }

    /// Returns accepted records (possibly empty). Drops counted by cause.
    pub fn accept(&mut self, datagram: &[u8], src_ip: &str, key: &[u8; 32], now: f64) -> Vec<PenRecord> {
        if datagram.len() < 27 || datagram[0] != MAGIC {
            self.drops.bad_magic += 1; return vec![];
        }
        let sid = u32::from_le_bytes(datagram[2..6].try_into().unwrap());
        if sid != self.session_id { self.drops.bad_session += 1; return vec![]; }
        if src_ip != self.control_peer_ip { self.drops.wrong_ip += 1; return vec![]; }
        let seq = u32::from_le_bytes(datagram[6..10].try_into().unwrap());
        if seq <= self.highest_seq { self.drops.old_seq += 1; return vec![]; }
        let recs = match decrypt_datagram(key, datagram) {
            Ok((_, _, r)) => r, Err(_) => { self.drops.bad_tag += 1; return vec![]; }
        };
        self.highest_seq = seq;
        self.last_datagram_at = Some(now);
        let mut out = vec![];
        for r in recs {
            let raw = encode_record(&r);
            if let Some(last) = self.last_t_ms { if r.t_ms < last { self.drops.old_tms += 1; continue; } }
            if self.last_record_bytes == Some(raw) { self.drops.identical += 1; continue; }
            self.last_t_ms = Some(r.t_ms);
            self.last_record_bytes = Some(raw);
            out.push(r);
        }
        for r in &out {
            match r.phase {
                0 => self.pen_in_range = true,
                1 | 2 => { self.pen_is_down = true; self.pen_in_range = true; }
                3 => self.pen_is_down = false,
                4 | 5 => { self.pen_is_down = false; self.pen_in_range = false; }
                _ => {}
            }
        }
        out
    }

    /// Watchdog: Some("up+leave") after 750 ms down-silence,
    /// Some("leave") after 1000 ms hover-silence, else None.
    pub fn watchdog(&mut self, now: f64) -> Option<&'static str> {
        let last = self.last_datagram_at?;
        let idle_ms = (now - last) * 1000.0;
        if self.pen_is_down && idle_ms >= 750.0 {
            self.pen_is_down = false; self.pen_in_range = false;
            return Some("up+leave");
        }
        if self.pen_in_range && idle_ms >= 1000.0 {
            self.pen_in_range = false;
            return Some("leave");
        }
        None
    }
}

// ---------------------------------------------------------- control (spec 4)

/// Validate a known control message. Unknown types are NOT errors (ignored).
pub fn validate_control(t: &str, proto: Option<u32>, phase: Option<&str>,
                        id: Option<&str>, mode: Option<&str>, has_fields: &[&str]) -> Vec<&'static str> {
    const KNOWN: &[&str] = &["challenge","hello","welcome","error","bye",
        "pair_request","pair_ok","pair_fail","ping","pong","surface",
        "button","button_state","profile","speak"];
    if !KNOWN.contains(&t) { return vec![]; }
    let mut errs = vec![];
    match t {
        "hello" => {
            if proto != Some(1) { errs.push("bad proto"); }
            for f in ["device_id","device_name","app_version","auth","caps","pen_area"] {
                if !has_fields.contains(&f) { errs.push("missing field"); break; }
            }
        }
        "button" => {
            if phase != Some("down") && phase != Some("up") { errs.push("bad button phase"); }
            if id.unwrap_or("").is_empty() { errs.push("missing button id"); }
        }
        "pair_request" => {
            if mode != Some("qr") && mode != Some("pin") { errs.push("bad pair mode"); }
        }
        _ => {}
    }
    errs
}

// ---------------------------------------------------------- pairing (spec 3)

pub struct PairingWindow { pub open_at: Option<f64>, pub now: f64, pub failures: u32 }

impl PairingWindow {
    pub fn is_open(&self) -> bool {
        match self.open_at {
            Some(t) => self.now - t < 120.0 && self.failures < 5,
            None => false,
        }
    }
    /// Returns "retry" or "closed".
    pub fn record_failure(&mut self) -> &'static str {
        self.failures += 1;
        if self.failures >= 5 || !self.is_open() { self.open_at = None; return "closed"; }
        "retry"
    }
    pub fn record_success(&mut self) { self.open_at = None; }
}

/// Reconnect schedule: 0.5, 1, 2, 4 s then every 5 s (spec 3).
pub fn reconnect_delay(attempt: u32) -> f64 {
    match attempt { 0 => 0.5, 1 => 1.0, 2 => 2.0, 3 => 4.0, _ => 5.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn secret() -> [u8; 32] { let mut s = [0u8; 32]; for i in 0..32 { s[i] = i as u8; } s }
    fn salt() -> [u8; 16] { let mut s = [0u8; 16]; for i in 0..16 { s[i] = 0xa0 + i as u8; } s }

    #[test] fn pen_key_vector() {
        let key = derive_pen_key(&secret(), &salt());
        let hex: String = key.iter().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(hex, "0fcc4ff1babc2af33fbfa107f14f14632e88eafcc4624d87f11072ca74e777d9");
    }
    #[test] fn datagram_vector() {
        let key = derive_pen_key(&secret(), &salt());
        let r = PenRecord { phase: 1, eraser: false, barrel1: false, barrel2: false,
            x: 0x8000, y: 0x4000, pressure: 0x2000, tilt_x: 15, tilt_y: -10,
            distance: 0, t_ms: 1234 };
        let dg = encrypt_datagram(&key, 1093482, 1, &[r]);
        let hex: String = dg.iter().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(hex, "c1006aaf1000010000008042b5e368236419bdc1a62d3f80e2e3c0b136f1c90ea1278848a0bff1f7232524");
        let (sid, seq, recs) = decrypt_datagram(&key, &dg).unwrap();
        assert_eq!((sid, seq), (1093482, 1));
        assert_eq!(recs[0], r);
    }
    #[test] fn auth_and_proof_vectors() {
        let s = secret();
        let mut nonce = [0u8; 16]; for i in 0..16 { nonce[i] = i as u8; }
        let id = "5b0f6c1e-7d0a-4a55-9d3e-0c6f1a2b3c4d";
        assert_eq!(compute_auth(&s, &nonce, id), "DdLGZwJqsXzvI3MQKJX9vL5d9LJnGT4eYPCx6kTgps8=");
        let mut fp = [0u8; 32]; for i in 0..32 { fp[i] = 0x20 + i as u8; }
        assert_eq!(compute_pin_proof("12345678", &fp, &nonce, id),
                   "XDbvfPmZHgCAs32kb3NRxh11K7eINs+TazjCRsV116Y=");
    }
    #[test] fn mapping_vectors() {
        let cases = [
            ((2100.0,1600.0,0u16,(0.5,0.5)), (960,540)),
            ((2100.0,1600.0,0,(0.0,0.0)), (0,0)),
            ((2100.0,1600.0,0,(0.25,0.25)), (480,174)),
            ((2100.0,1600.0,0,(1.0,1.0)), (1919,1079)),
            ((1600.0,2100.0,90,(0.25,0.75)), (480,174)),
        ];
        for ((pw,ph,rot,(x,y)), (ex,ey)) in cases {
            assert_eq!(map_pen_to_px(x,y,pw,ph,0.0,0.0,1920.0,1080.0,(0.0,0.0,1.0,1.0),true,rot),(ex,ey));
        }
    }
    #[test] fn curve_vectors() {
        let cases = [
            ([0.25,0.25,0.75,0.75],[0.25,0.5,0.75],[0.25,0.5,0.75]),
            ([0.10,0.40,0.50,0.90],[0.25,0.5,0.75],[0.4946,0.7532,0.9145]),
            ([0.50,0.10,0.90,0.50],[0.25,0.5,0.75],[0.0781,0.2213,0.4622]),
        ];
        for (c, ps, ws) in cases {
            for (p, w) in ps.into_iter().zip(ws) {
                assert!((apply_pressure_curve(&c, p) - w).abs() <= 0.002, "{c:?} {p}");
            }
        }
    }
    #[test] fn receiver_rules() {
        let key = derive_pen_key(&secret(), &salt());
        let mk = |phase: u8, t: u32| PenRecord { phase, eraser: false, barrel1: false,
            barrel2: false, x: 100, y: 100, pressure: 100, tilt_x: 0, tilt_y: 0,
            distance: 0, t_ms: t };
        let mut rx = PenReceiver::new(7, "192.168.1.5");
        let d1 = encrypt_datagram(&key, 7, 1, &[mk(0, 100)]);
        assert_eq!(rx.accept(&d1, "192.168.1.5", &key, 10.0).len(), 1);
        assert!(rx.accept(&d1, "192.168.1.5", &key, 10.1).is_empty()); // replay seq
        assert_eq!(rx.drops.old_seq, 1);
        let d2 = encrypt_datagram(&key, 7, 2, &[mk(2, 50)]); // older t_ms
        assert!(rx.accept(&d2, "192.168.1.5", &key, 10.2).is_empty());
        assert_eq!(rx.drops.old_tms, 1);
        let mut bad = encrypt_datagram(&key, 7, 3, &[mk(2, 200)]);
        *bad.last_mut().unwrap() ^= 1;
        assert!(rx.accept(&bad, "192.168.1.5", &key, 10.3).is_empty());
        assert_eq!(rx.drops.bad_tag, 1);
    }
    #[test] fn pairing_and_reconnect() {
        let mut w = PairingWindow { open_at: Some(0.0), now: 119.0, failures: 0 };
        assert!(w.is_open());
        w.now = 121.0;
        assert!(!w.is_open());
        assert_eq!([reconnect_delay(0),reconnect_delay(1),reconnect_delay(2),
                    reconnect_delay(3),reconnect_delay(4),reconnect_delay(9)],
                   [0.5,1.0,2.0,4.0,5.0,5.0]);
    }
}
