//! Shared application state and the actions the GUI, hotkey and watchers
//! perform on it.
//!
//! Locking rules (they are what keeps this deadlock-free):
//! - every field is a std `Mutex`, locked through `lock_ok`, held only for
//!   a few statements and NEVER across an `.await`;
//! - no function holds two of these locks at the same time.
//!
//! Because nothing here awaits, every action is safe to call from any
//! thread: the OS hotkey thread, the GUI thread, or a runtime task.

use std::sync::{Arc, Mutex};

use chiz_core::action::{ActionRunner, Effect};
use chiz_core::failsafe::{Failsafe, PauseReason};

use crate::config::Config;
use crate::hub::{Active, Hub};
use crate::inject::{Injector, ViewConfig};
use crate::profiles::ProfileSet;
use crate::store::PairedStore;
use crate::util::{now_secs, LockExt};

pub struct App {
    pub cfg: Config,
    /// Own certificate fingerprint: binds PIN proofs, feeds QR and mDNS.
    pub fp: [u8; 32],
    pub hostname: String,
    pub hub: Mutex<Hub>,
    pub profiles: Mutex<ProfileSet>,
    pub runner: Mutex<ActionRunner>,
    pub injector: Mutex<Injector>,
    pub failsafe: Mutex<Failsafe>,
}

impl App {
    pub fn new(
        cfg: Config,
        fp: [u8; 32],
        hostname: String,
        paired: PairedStore,
        profiles: ProfileSet,
    ) -> Arc<Self> {
        let mut failsafe = Failsafe::default();
        if let Err(e) = failsafe.set_hotkey(&cfg.panic_hotkey) {
            eprintln!("bad panic_hotkey {e:?}, keeping the default");
        }
        let view = ViewConfig {
            union_w: cfg.desktop_w,
            union_h: cfg.desktop_h,
            area: cfg.area,
            keep_aspect: cfg.keep_aspect,
            rotation: cfg.rotation,
            curve: cfg.pressure_curve,
        };
        Arc::new(Self {
            cfg,
            fp,
            hostname,
            hub: Mutex::new(Hub::new(paired)),
            profiles: Mutex::new(profiles),
            runner: Mutex::new(ActionRunner::default()),
            injector: Mutex::new(Injector::new(view)),
            failsafe: Mutex::new(failsafe),
        })
    }

    /// Apply key/mouse effects to the OS (other effect kinds are handled by
    /// the session that produced them).
    pub fn drive(&self, effects: &[Effect]) {
        if !effects
            .iter()
            .any(|e| matches!(e, Effect::Key { .. } | Effect::Mouse { .. }))
        {
            return;
        }
        let mut inj = self.injector.lock_ok();
        inj.ensure(now_secs());
        for fx in effects {
            match fx {
                Effect::Key { key, down: true } => inj.key(key, true),
                Effect::Key { key, down: false } => inj.key_release(key),
                Effect::Mouse { button, down } => inj.mouse(button, *down),
                _ => {}
            }
        }
    }

    /// Lift the pen and release every held key/button (spec 4, 8). Called on
    /// session end, profile change, pause and panic.
    pub fn release_input(&self, why: &str) {
        let held = self.runner.lock_ok().release_all();
        self.drive(&held);
        self.injector.lock_ok().force_leave();
        eprintln!("released all input ({why})");
    }

    /// Queue a message for the connected tablet, if any.
    pub fn send_to_tablet(&self, v: &serde_json::Value) {
        if let Some(a) = self.hub.lock_ok().active() {
            a.send(v);
        }
    }

    /// Tell the tablet which profile is active now.
    pub fn push_profile(&self, reason: &str) {
        let msg = self.profiles.lock_ok().current_wire(reason);
        if let Some(m) = msg {
            self.send_to_tablet(&m);
        }
    }

    /// Close a session that the hub has already removed, telling the tablet
    /// why with an `error` frame first (`code`: `replaced` stops a stale
    /// tablet instance from fighting its successor; `not_paired` after a
    /// forget).
    pub fn end_session(&self, a: Active, code: &str, why: &str) {
        a.send(&crate::wire::error(code, why));
        a.close(why);
        self.release_input(why);
    }

    // ------------------------------------------------------------- failsafe

    /// THE emergency off switch. Synchronous and lock-light, so it works
    /// from the OS hotkey thread even if every network task is wedged.
    pub fn emergency_release(&self) {
        let newly = self.failsafe.lock_ok().emergency_release();
        // Release even if already paused: a stuck modifier must never
        // survive the panic switch.
        self.release_input("panic");
        if newly {
            banner("Emergency release: pen paused, everything released.");
        }
    }

    pub fn pause(&self) {
        if self.failsafe.lock_ok().pause(PauseReason::Manual) {
            self.release_input("pause");
            banner("Pen input paused. Buttons keep working.");
        }
    }

    pub fn resume(&self) {
        self.failsafe.lock_ok().resume();
        banner("Pen input resumed.");
    }

    pub fn toggle_pause(&self) {
        let paused = self.failsafe.lock_ok().paused().is_some();
        if paused {
            self.resume()
        } else {
            self.pause()
        }
    }

    // ------------------------------------------------------------- pairing / profiles

    /// Open the 120 s pairing window and render its QR beside the identity.
    pub fn open_pairing(&self) {
        let hosts = crate::server::lan_hosts();
        let host_refs: Vec<&str> = hosts.iter().map(String::as_str).collect();
        let (uri, pin) = {
            let mut hub = self.hub.lock_ok();
            let (_token, pin) = hub.pairing.open(now_secs());
            (
                hub.pairing
                    .qr_uri(&host_refs, self.cfg.control_port, &self.fp),
                pin,
            )
        };
        eprintln!("pairing open for 120 s, PIN {pin}, addresses {hosts:?}");
        eprintln!("pairing QR: {uri}");
        match crate::pairing::qr_jpg(&uri) {
            Ok((jpg, _ascii)) => {
                let path = crate::identity::data_dir().join("pair_qr.jpg");
                if let Err(e) = std::fs::write(&path, &jpg) {
                    eprintln!("could not save the pairing QR: {e}");
                }
            }
            Err(e) => eprintln!("QR render failed: {e}"),
        }
    }

    pub fn forget_tablet(&self, device_id: &str) {
        let live = self.hub.lock_ok().forget(device_id);
        if let Some(a) = live {
            self.end_session(a, "not_paired", "this tablet was forgotten on the PC");
        }
        banner(&format!("Tablet forgotten ({device_id})."));
    }

    /// Manual profile lock; `None` returns to following the foreground app.
    pub fn lock_profile(&self, target: Option<&str>) {
        let switched = self.profiles.lock_ok().switch(target.unwrap_or("auto"));
        if let Some((id, reason)) = switched {
            self.release_input("profile change");
            self.push_profile(reason);
            eprintln!("profile {id} ({reason})");
        }
    }

    /// Foreground watcher callback: switch profile if the app change matters.
    pub fn set_foreground(&self, app: Option<String>) {
        let changed = self.profiles.lock_ok().set_foreground(app);
        if changed {
            self.release_input("profile change");
            self.push_profile("app");
        }
    }
}

/// User-visible status line; lands in the log.
pub fn banner(msg: &str) {
    eprintln!("CHIZ: {msg}");
}

// ------------------------------------------------------------------ OS hooks

/// Register the global emergency hotkey. Failure is reported, never fatal.
pub fn install_panic_hotkey(app: &Arc<App>) {
    let keys = app.failsafe.lock_ok().panic_hotkey.clone();
    let a = Arc::clone(app);
    let hook = move || a.emergency_release();
    #[cfg(unix)]
    let ok = chiz_platform_linux::spawn_panic_hook(&keys, hook);
    #[cfg(windows)]
    let ok = chiz_platform_windows::spawn_panic_hook(&keys, hook);
    if ok {
        eprintln!("panic hotkey armed: {}", keys.join("+"));
    } else {
        banner(&format!(
            "panic hotkey {} NOT available (no X11 session, or another app owns it). \
             Use the Emergency release button or a 3-finger tap on the tablet strip.",
            keys.join("+")
        ));
    }
}

/// Poll the focused app; commit a change once it is stable for ~300 ms
/// (spec 8). Chiz's own window never counts, so clicking the Chiz GUI does
/// not flip the strip to the default profile.
pub fn spawn_foreground_watcher(app: Arc<App>) {
    let own = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().to_lowercase()));
    let started = std::thread::Builder::new()
        .name("chiz-foreground".into())
        .spawn(move || {
            #[cfg(unix)]
            let mut probe = chiz_platform_linux::x11::ForegroundProbe::default();
            #[cfg_attr(windows, allow(unused_mut))]
            let mut read = move || -> Option<String> {
                #[cfg(unix)]
                {
                    probe.foreground_app()
                }
                #[cfg(windows)]
                {
                    chiz_platform_windows::foreground_app_name()
                }
            };
            let mut committed: Option<String> = None;
            let (mut candidate, mut seen) = (None::<String>, 0u32);
            loop {
                std::thread::sleep(std::time::Duration::from_millis(150));
                let now = read();
                let is_own = match (&now, &own) {
                    (Some(n), Some(o)) => n.trim_end_matches(".exe") == o,
                    _ => false,
                };
                if is_own || now == committed {
                    candidate = None;
                    seen = 0;
                    continue;
                }
                if now == candidate {
                    seen += 1;
                } else {
                    candidate = now.clone();
                    seen = 1;
                }
                if seen >= 2 {
                    committed = now.clone();
                    candidate = None;
                    seen = 0;
                    app.set_foreground(now);
                }
            }
        });
    if started.is_err() {
        eprintln!("could not start the foreground watcher; auto profile switching is off");
    }
}

#[cfg(test)]
pub mod testing {
    use super::*;

    /// An App with in-memory storage and the repo's/built-in profiles.
    pub fn app() -> Arc<App> {
        App::new(
            Config::default(),
            [0xab; 32],
            "testpc".into(),
            PairedStore::in_memory(),
            crate::profiles::load(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The panic switch runs on a foreign OS thread with no runtime and
    /// still pauses + releases (it must never need the async runtime).
    #[test]
    fn panic_from_foreign_thread() {
        let app = testing::app();
        let a = Arc::clone(&app);
        std::thread::spawn(move || a.emergency_release())
            .join()
            .unwrap();
        assert_eq!(app.failsafe.lock_ok().paused(), Some(PauseReason::Panic));
        app.emergency_release(); // idempotent
        app.toggle_pause();
        assert_eq!(app.failsafe.lock_ok().paused(), None);
    }

    #[test]
    fn profile_lock_and_foreground() {
        let app = testing::app();
        app.set_foreground(Some("krita".into()));
        assert_eq!(
            app.profiles.lock_ok().current_id().as_deref(),
            Some("krita")
        );
        app.lock_profile(Some("default"));
        assert_eq!(
            app.profiles.lock_ok().current_id().as_deref(),
            Some("default")
        );
        app.lock_profile(None);
        assert_eq!(
            app.profiles.lock_ok().current_id().as_deref(),
            Some("krita")
        );
    }
}
