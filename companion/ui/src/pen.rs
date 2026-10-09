//! UDP pen path (spec 5, 8) and the stuck-pen watchdog.
//!
//! The UDP task never waits on control, UI or disk work (spec 2): per
//! datagram it takes the hub lock only long enough to authenticate and
//! decrypt against the active session, then injects.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use chiz_core::{PenReceiver, PenRecord};
use tokio::net::UdpSocket;

use crate::app::{banner, App};
use crate::util::{now_secs, LockExt};

pub async fn udp_task(app: Arc<App>, sock: UdpSocket) {
    let mut buf = [0u8; 2048];
    loop {
        match sock.recv_from(&mut buf).await {
            Ok((n, src)) => on_datagram(&app, &buf[..n], src.ip()),
            Err(e) => {
                // Windows reports ICMP port-unreachable as a recv error;
                // never spin on it.
                eprintln!("pen UDP receive error: {e}");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
}

/// Filter one datagram (session, source IP, replay, tag) and inject it.
pub fn on_datagram(app: &App, dg: &[u8], src: IpAddr) {
    let Some(sid) = dg
        .get(2..6)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    else {
        return;
    };
    {
        // Pause gate: drop pen records; buttons are unaffected.
        let mut fs = app.failsafe.lock_ok();
        if fs.drop_pen() {
            fs.leave_pending = false;
            return;
        }
    }
    let now = now_secs();
    let (records, aspect) = {
        let mut hub = app.hub.lock_ok();
        let Some(a) = hub.active_mut() else {
            return; // no session: drop
        };
        if a.sid != sid {
            return; // stale or foreign session id (spec 4/5)
        }
        let key = a.pen_key;
        (accept(&mut a.rx, dg, src, &key, now), a.aspect)
    };
    if records.is_empty() {
        return;
    }
    let eraser_forced = app.runner.lock_ok().eraser_on;
    {
        let mut inj = app.injector.lock_ok();
        inj.ensure(now);
        // Pipeline order (spec 8): rotate+map -> pressure curve ->
        // eraser-toggle -> inject.
        for r in records {
            inj.pen(
                r.phase,
                r.x,
                r.y,
                r.pressure,
                r.distance,
                r.tilt_x,
                r.tilt_y,
                r.eraser || eraser_forced,
                r.barrel1,
                r.barrel2,
                aspect,
            );
        }
    }
    app.failsafe.lock_ok().note_clean_input();
}

fn accept(
    rx: &mut PenReceiver,
    dg: &[u8],
    src: IpAddr,
    key: &[u8; 32],
    now: f64,
) -> Vec<PenRecord> {
    rx.accept(dg, &src.to_string(), key, now)
}

/// 100 ms ticker: lift a pen whose stream went silent (750 ms down /
/// 1000 ms hover). Three repairs in a row without clean input auto-pause.
pub async fn watchdog_task(app: Arc<App>) {
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        watchdog_tick(&app);
    }
}

pub fn watchdog_tick(app: &App) {
    let tripped = {
        let mut hub = app.hub.lock_ok();
        hub.active_mut().and_then(|a| a.rx.watchdog(now_secs()))
    };
    let Some(action) = tripped else {
        return;
    };
    eprintln!("pen watchdog: {action} (stream went silent)");
    app.injector.lock_ok().force_leave();
    let paused = app.failsafe.lock_ok().note_stuck_trip().is_some();
    if paused {
        app.release_input("auto-pause");
        banner("Pen auto-paused after repeated stuck input. Resume from the Chiz window.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testing;
    use crate::store::PairedTablet;
    use chiz_core::failsafe::PauseReason;

    fn rec(t: u32) -> PenRecord {
        PenRecord {
            phase: 2,
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

    /// An app with tablet "T" connected from 127.0.0.1; returns (sid, key).
    fn connected() -> (std::sync::Arc<App>, u32, [u8; 32]) {
        let app = testing::app();
        let mut hub = app.hub.lock_ok();
        hub.paired.insert(
            "T",
            PairedTablet {
                secret: [4; 32],
                name: "t".into(),
            },
        );
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        let conn = hub.admit(ip, 0.0, false).unwrap();
        let nonce = [1u8; 16];
        let auth = chiz_core::compute_auth(&[4; 32], &nonce, "T");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let acc = hub
            .hello(conn, "T", &auth, &nonce, ip, None, 0.0, tx)
            .ok()
            .unwrap();
        let key = hub.active().unwrap().pen_key;
        drop(hub);
        (app, acc.sid, key)
    }

    fn highest(app: &App) -> u32 {
        app.hub.lock_ok().active().unwrap().rx.highest_seq
    }

    #[test]
    fn accepts_active_session_rejects_others() {
        let (app, sid, key) = connected();
        let lo: IpAddr = "127.0.0.1".parse().unwrap();
        on_datagram(
            &app,
            &chiz_core::encrypt_datagram(&key, sid, 1, &[rec(10)]),
            lo,
        );
        assert_eq!(highest(&app), 1);
        // Wrong session id: ignored before decryption.
        on_datagram(
            &app,
            &chiz_core::encrypt_datagram(&key, sid + 1, 2, &[rec(20)]),
            lo,
        );
        assert_eq!(highest(&app), 1);
        // Wrong source IP: counted as a drop.
        let other: IpAddr = "10.0.0.9".parse().unwrap();
        on_datagram(
            &app,
            &chiz_core::encrypt_datagram(&key, sid, 3, &[rec(30)]),
            other,
        );
        assert_eq!(highest(&app), 1);
        assert!(app.hub.lock_ok().active().unwrap().dropped() >= 1);
        // Garbage never panics.
        on_datagram(&app, &[1, 2, 3], lo);
    }

    #[test]
    fn paused_pen_is_dropped() {
        let (app, sid, key) = connected();
        app.failsafe.lock_ok().pause(PauseReason::Manual);
        let lo: IpAddr = "127.0.0.1".parse().unwrap();
        on_datagram(
            &app,
            &chiz_core::encrypt_datagram(&key, sid, 1, &[rec(10)]),
            lo,
        );
        assert_eq!(highest(&app), 0);
        assert!(!app.failsafe.lock_ok().leave_pending);
    }
}
