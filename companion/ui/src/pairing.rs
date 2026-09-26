//! Pairing window + QR/PIN verification + QR render (spec 3).
//!
//! Flow: user action on the PC opens a 120 s window holding a 16-byte pair
//! token and an 8-digit PIN. The PC shows the QR URI (`chiz://pair?...`,
//! rendered below as PBM + terminal ASCII), the PIN, and its addresses.
//! The tablet answers with `pair_request` (`qr`: token, `pin`: HMAC proof);
//! success returns `pair_ok` with a fresh 32-byte `device_secret`, failure
//! returns `pair_fail` with attempts left. Window closes on success,
//! timeout, or 5 failures. Outside a window: `error not_paired`.

use chiz_core::{compute_pin_proof, qr, PairingWindow};

pub struct PairingState {
    pub window: PairingWindow,
    pub token: [u8; 16],
    pub pin: String,
}

impl PairingState {
    pub fn closed() -> Self {
        Self {
            window: PairingWindow {
                open_at: None,
                now: 0.0,
                failures: 0,
            },
            token: [0u8; 16],
            pin: String::new(),
        }
    }

    /// Open a window (deliberate user action: tray "Pair new tablet").
    /// Returns (token, pin) for display.
    pub fn open(&mut self, now: f64) -> ([u8; 16], String) {
        let mut token = [0u8; 16];
        crate::os_random(&mut token);
        let mut raw = [0u8; 4];
        crate::os_random(&mut raw);
        let pin = format!("{:08}", u32::from_le_bytes(raw) % 100_000_000);
        self.window.open_at = Some(now);
        self.window.now = now;
        self.window.failures = 0;
        self.token = token;
        self.pin = pin.clone();
        (token, pin)
    }

    pub fn is_open(&mut self, now: f64) -> bool {
        self.window.now = now;
        self.window.is_open()
    }

    /// QR-mode proof: constant-time token compare.
    pub fn verify_qr(&self, token_b64: &str) -> bool {
        let Ok(raw) = crate::base64_b64_decode(token_b64) else {
            return false;
        };
        crate::ct_eq(&raw, &self.token)
    }

    /// PIN-mode proof: `HMAC(PIN, "chiz-pair-v1"||fp||nonce||device_id)`.
    /// The tablet binds the fingerprint it *observed*, so a MITM cert fails.
    pub fn verify_pin(&self, proof_b64: &str, fp_seen: &[u8; 32], nonce: &[u8; 16], device_id: &str) -> bool {
        let expect = compute_pin_proof(&self.pin, fp_seen, nonce, device_id);
        crate::ct_eq(expect.as_bytes(), proof_b64.as_bytes())
    }

    pub fn attempts_left(&self) -> u32 {
        5u32.saturating_sub(self.window.failures)
    }

    pub fn qr_uri(&self, hosts: &[&str], port: u16, fp: &[u8; 32]) -> String {
        qr::build_qr_uri(hosts, port, fp, &self.token)
    }
}

/// Render a QR URI to PBM (P4 bitmap, scale modules x4 + quiet zone 4) for
/// the pairing screen, plus terminal ASCII for logs/tests.
pub fn qr_pbm(uri: &str) -> Result<(Vec<u8>, String), String> {
    use qrcodegen::{QrCode, QrCodeEcc};
    let qr = QrCode::encode_text(uri, QrCodeEcc::Medium).map_err(|e| format!("qr: {e:?}"))?;
    let n = qr.size() as usize;
    let border = 4usize;
    let scale = 4usize;
    let dim = (n + border * 2) * scale;
    let row_bytes = dim.div_ceil(8);
    let mut bits = vec![0u8; row_bytes * dim];
    let mut ascii = String::new();
    for y in 0..n + border * 2 {
        let mut line = String::new();
        for x in 0..n + border * 2 {
            let dark = x >= border
                && y >= border
                && (x - border) < n
                && (y - border) < n
                && qr.get_module((x - border) as i32, (y - border) as i32);
            for _ in 0..scale {
                if dark {
                    let px = x * scale;
                    let py = (y * scale) as usize;
                    for sy in 0..scale {
                        let yy = py + sy;
                        let xx = px;
                        for sx in 0..scale {
                            let bit = 7 - ((xx + sx) % 8);
                            bits[yy * row_bytes + (xx + sx) / 8] |= 1 << bit;
                        }
                    }
                }
            }
            line.push(if dark { '#' } else { ' ' });
        }
        ascii.push_str(&line);
        ascii.push('\n');
    }
    let mut pbm = format!("P4\n{dim} {dim}\n").into_bytes();
    pbm.extend_from_slice(&bits);
    Ok((pbm, ascii))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_proof_matches_spec_vector() {
        // Same values as protocol/test_vectors.json pin_proof_vector.
        let mut fp = [0u8; 32];
        for i in 0..32 {
            fp[i] = 0x20 + i as u8;
        }
        let mut nonce = [0u8; 16];
        for i in 0..16 {
            nonce[i] = i as u8;
        }
        let st = PairingState {
            window: PairingWindow {
                open_at: Some(0.0),
                now: 0.0,
                failures: 0,
            },
            token: [0u8; 16],
            pin: "12345678".into(),
        };
        assert!(st.verify_pin(
            "XDbvfPmZHgCAs32kb3NRxh11K7eINs+TazjCRsV116Y=",
            &fp,
            &nonce,
            "5b0f6c1e-7d0a-4a55-9d3e-0c6f1a2b3c4d"
        ));
        assert!(!st.verify_pin("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=", &fp, &nonce, "5b0f6c1e-7d0a-4a55-9d3e-0c6f1a2b3c4d"));
        // MITM fingerprint fails even with the right PIN.
        let mut other = fp;
        other[0] ^= 1;
        assert!(!st.verify_pin(
            "XDbvfPmZHgCAs32kb3NRxh11K7eINs+TazjCRsV116Y=",
            &other,
            &nonce,
            "5b0f6c1e-7d0a-4a55-9d3e-0c6f1a2b3c4d"
        ));
    }

    #[test]
    fn qr_token_roundtrip() {
        let mut st = PairingState::closed();
        let (tok, pin) = st.open(100.0);
        assert_eq!(pin.len(), 8);
        assert!(st.is_open(101.0));
        assert!(!st.is_open(221.0)); // 120 s window
        let uri = st.qr_uri(&["192.168.1.10"], 47800, &[0xabu8; 32]);
        let back = qr::parse_qr_uri(&uri).unwrap();
        assert_eq!(back.token, tok);
        assert_eq!(back.hosts, vec!["192.168.1.10"]);
    }

    #[test]
    fn qr_renders_scannable_shape() {
        let uri = "chiz://pair?v=1&h=192.168.1.10&p=47800&fp=AA&t=AA";
        let (pbm, ascii) = qr_pbm(uri).unwrap();
        assert!(pbm.starts_with(b"P4\n"));
        assert!(ascii.contains('#') && ascii.contains(' '));
        // Quiet zone: first row all light.
        assert!(ascii.lines().next().unwrap().trim().is_empty());
    }
}
