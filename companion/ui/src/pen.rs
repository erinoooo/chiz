//! UDP pen loop: one receive task -> PenReceiver -> injection task.
//! Spec section 5 (receiver rules) + section 8 (pen pipeline order).
//!
//! Wiring: `tokio::net::UdpSocket` bound on the pen port feeds datagrams to
//! `chiz_core::PenReceiver::accept` (magic/session/IP/seq/tag/t_ms/identical
//! filtering with drop counters shown on the status screen). Accepted records
//! go through rotate+map -> pressure curve -> eraser-toggle -> `inject_pen`
//! on the platform layer, after `PenState` repair (chiz-core). Watchdog task
//! injects up+leave (750 ms down-silence) or leave (1000 ms hover-silence).
//!
//! Threading (spec 2): this UDP task never touches disk, DNS, UI, or the
//! control channel. Decoded records cross to the injection task over a
//! bounded channel; on overflow the oldest move/hover is dropped (tablet
//! mirrors this on its sender queue).

use chiz_core::{PenReceiver, PenRecord};

/// Outcome of one datagram for the status-screen counters.
#[allow(dead_code)]
#[derive(Debug, Default)]
pub struct PenStats {
    pub accepted: u64,
    pub dropped: u64,
}

/// Filter one datagram through the receiver; pure wrapper so the async loop
/// stays thin and this stays unit-testable without sockets.
#[allow(dead_code)]
pub fn handle_datagram(
    rx: &mut PenReceiver,
    datagram: &[u8],
    src_ip: &str,
    pen_key: &[u8; 32],
    now_secs: f64,
) -> (Vec<PenRecord>, PenStats) {
    let before = rx.drops.bad_magic
        + rx.drops.bad_session
        + rx.drops.bad_tag
        + rx.drops.wrong_ip
        + rx.drops.old_seq
        + rx.drops.old_tms
        + rx.drops.identical;
    let recs = rx.accept(datagram, src_ip, pen_key, now_secs);
    let after = rx.drops.bad_magic
        + rx.drops.bad_session
        + rx.drops.bad_tag
        + rx.drops.wrong_ip
        + rx.drops.old_seq
        + rx.drops.old_tms
        + rx.drops.identical;
    (
        recs,
        PenStats {
            accepted: 0, // filled by caller from recs.len()
            dropped: (after - before) as u64,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chiz_core::{derive_pen_key, encrypt_datagram};

    fn key() -> [u8; 32] {
        let mut s = [0u8; 32];
        for i in 0..32 {
            s[i] = i as u8;
        }
        let mut salt = [0u8; 16];
        for i in 0..16 {
            salt[i] = 0xa0 + i as u8;
        }
        derive_pen_key(&s, &salt)
    }

    fn rec(phase: u8, t: u32) -> PenRecord {
        PenRecord {
            phase,
            eraser: false,
            barrel1: false,
            barrel2: false,
            x: 1000,
            y: 2000,
            pressure: 30000,
            tilt_x: 0,
            tilt_y: 0,
            distance: 0,
            t_ms: t,
        }
    }

    #[test]
    fn udp_accept_and_replay_counts() {
        let k = key();
        let mut rx = PenReceiver::new(7, "192.168.1.5");
        let d1 = encrypt_datagram(&k, 7, 1, &[rec(0, 100)]);
        let (r, s) = handle_datagram(&mut rx, &d1, "192.168.1.5", &k, 10.0);
        assert_eq!(r.len(), 1);
        assert_eq!(s.dropped, 0);
        let (r2, s2) = handle_datagram(&mut rx, &d1, "192.168.1.5", &k, 10.1);
        assert!(r2.is_empty());
        assert_eq!(s2.dropped, 1); // replayed seq
    }

    #[test]
    fn udp_wrong_ip_counted() {
        let k = key();
        let mut rx = PenReceiver::new(7, "192.168.1.5");
        let d = encrypt_datagram(&k, 7, 1, &[rec(2, 100)]);
        let (r, s) = handle_datagram(&mut rx, &d, "10.9.9.9", &k, 10.0);
        assert!(r.is_empty());
        assert_eq!(s.dropped, 1);
    }
}
