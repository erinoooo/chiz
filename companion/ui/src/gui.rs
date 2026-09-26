//! Native GUI runner (eframe): launching the companion opens the main
//! window by default. `--headless`, `CHIZ_HEADLESS=1`, or no display server
//! runs server-only (logs + mDNS + tablet only).
//!
//! The screens in `screens.rs` stay I/O-free: each frame syncs them from
//! `Shared` with non-blocking locks, renders, then executes any
//! [PendingAction](super::screens::PendingAction)s the buttons queued.

use std::sync::Arc;
use std::time::Duration;

use super::screens::{CompanionApp, PendingAction};
use super::{broadcast_profile, emergency_release, open_pairing, tray_action, Shared};
use chiz_core::failsafe::PauseReason;

pub struct GuiApp {
    rt: tokio::runtime::Handle,
    shared: Arc<Shared>,
    fp: [u8; 32],
    hostname: String,
    port: u16,
    app: CompanionApp,
}

impl GuiApp {
    pub fn new(
        rt: tokio::runtime::Handle,
        shared: Arc<Shared>,
        fp: [u8; 32],
        hostname: String,
        port: u16,
    ) -> Self {
        Self {
            rt,
            shared,
            fp,
            hostname,
            port,
            app: CompanionApp::default(),
        }
    }

    fn sync(&mut self) {
        // Sessions: dot + tablet name.
        if let Ok(s) = self.shared.sessions.try_lock() {
            let tabs = s.tablets();
            self.app.status.connected = !tabs.is_empty();
            self.app.status.reconnecting = false;
            self.app.status.tablet_name = tabs.join(", ");
        }
        // Drop counters across receivers.
        if let Ok(rx) = self.shared.pen_rx.try_lock() {
            self.app.status.dropped = rx
                .values()
                .map(|r| {
                    (r.drops.bad_magic
                        + r.drops.bad_session
                        + r.drops.bad_tag
                        + r.drops.wrong_ip
                        + r.drops.old_seq
                        + r.drops.old_tms
                        + r.drops.identical) as u64
                })
                .sum();
        }
        // Failsafe: pause flag + banner (std mutex: never blocks).
        {
            let fs = self.shared.failsafe.lock().expect("failsafe");
            self.app.status.paused = fs.paused().is_some();
            self.app.status.banner = match fs.paused() {
                Some(PauseReason::AutoStuck) => {
                    "Pen auto-paused after repeated stuck input. Resume when ready.".into()
                }
                Some(PauseReason::Panic) => "Emergency release active. Resume when ready.".into(),
                _ => String::new(),
            };
            if self.app.settings.panic_hotkey.is_empty() {
                self.app.settings.panic_hotkey = fs.panic_hotkey.join("+");
            }
        }
        // Pairing window: open flag, countdown, PIN.
        if let Ok(mut p) = self.shared.pairing.try_lock() {
            let open = p.is_open(super::now_secs());
            self.app.tablets.pairing_open = open;
            if open {
                let left = 120 - (super::now_secs() - p.window.open_at.unwrap_or(0.0)) as u64;
                self.app.tablets.countdown_s = left.min(120);
                self.app.tablets.pin = p.pin.clone();
            }
        }
        // Profiles: ids + manual lock.
        if let Ok(ps) = self.shared.profiles.try_lock() {
            self.app.profiles.ids = ps.profiles.iter().map(|p| p.id.clone()).collect();
            self.app.profiles.selected = ps.locked.clone();
        }
        // Paired tablets: id + stored name.
        if let Ok(paired) = self.shared.paired.try_lock() {
            let mut v: Vec<(String, String)> = paired
                .secrets
                .keys()
                .map(|id| {
                    (
                        id.clone(),
                        paired.names.get(id).cloned().unwrap_or_else(|| id.clone()),
                    )
                })
                .collect();
            v.sort();
            self.app.tablets.paired = v;
        }
        // Settings snapshot.
        self.app.mapping.monitors = vec!["primary".to_string(), "all".to_string()];
        self.app.settings.control_port = self.port;
        self.app.settings.wayland_note =
            std::env::var("XDG_SESSION_TYPE").as_deref() == Ok("wayland");
        let _ = self.hostname.clone();
    }

    fn exec(&self, action: PendingAction) {
        match action {
            PendingAction::Panic => emergency_release(&self.shared),
            PendingAction::PauseToggle => {
                let paused = self
                    .shared
                    .failsafe
                    .lock()
                    .expect("failsafe")
                    .paused()
                    .is_some();
                tray_action(&self.shared, if paused { "resume" } else { "pause" });
            }
            PendingAction::PairOpen => {
                self.rt.block_on(open_pairing(&self.shared, &self.fp));
            }
            PendingAction::ForgetTablet(id) => {
                {
                    let mut paired = self.shared.paired.blocking_lock();
                    paired.secrets.remove(&id);
                    paired.names.remove(&id);
                }
                // Single-tablet design: forgetting drops all live pen state.
                // The orphaned socket reaps on the 5 s liveness deadline.
                self.shared.sessions.blocking_lock().remove_device(&id);
                self.shared.pen_rx.blocking_lock().clear();
                self.shared.pen_key.blocking_lock().clear();
                let rel = self.shared.runner.blocking_lock().release_all();
                super::apply_effects(&rel);
                super::release_all("forget tablet");
                super::banner(&format!("Tablet forgotten ({id})."));
            }
            PendingAction::ProfileLock(target) => {
                let target = target.unwrap_or_else(|| "auto".to_string());
                let next = self.shared.profiles.blocking_lock().switch(&target);
                if let Some((id, reason)) = next {
                    let rel = self.shared.runner.blocking_lock().release_all();
                    super::apply_effects(&rel);
                    self.rt
                        .block_on(broadcast_profile(&self.shared, &reason));
                    super::banner(&format!("Profile locked: {id}"));
                }
            }
        }
    }
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.sync();
        self.app.show(ctx);
        for a in std::mem::take(&mut self.app.pending) {
            self.exec(a);
        }
        // Countdowns and reconnect states need repainting without input.
        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

/// Headless when asked, or when there is no display to open.
pub fn headless() -> bool {
    if std::env::args().any(|a| a == "--headless") {
        return true;
    }
    if std::env::var("CHIZ_HEADLESS").as_deref() == Ok("1") {
        return true;
    }
    #[cfg(unix)]
    {
        if std::env::var("DISPLAY").is_err() && std::env::var("WAYLAND_DISPLAY").is_err() {
            return true;
        }
    }
    false
}
