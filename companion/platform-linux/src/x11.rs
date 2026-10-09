//! X11 helpers: the global panic hotkey and foreground-app detection.
//!
//! Pure-Rust protocol client (no libxcb). Everything degrades to "not
//! available" when there is no X server (Wayland-only sessions): callers
//! get `None`/`false` and the UI says so; nothing here ever panics.

use std::fs;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt, GrabMode, ModMask, Window};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

/// Lock-state modifier bits we must ignore when matching the hotkey
/// (CapsLock = Lock, NumLock = Mod2).
const LOCK_BITS: [u16; 4] = [0, 0x02, 0x10, 0x12];

fn mod_mask(name: &str) -> Option<u16> {
    Some(match name {
        "shift" => 0x01,
        "ctrl" => 0x04,
        "alt" => 0x08,
        "meta" => 0x40,
        _ => return None,
    })
}

/// Lazily-connected foreground-window probe. Reconnects after errors.
#[derive(Default)]
pub struct ForegroundProbe {
    conn: Option<(RustConnection, usize, Atom, Atom)>,
}

impl ForegroundProbe {
    fn connect() -> Option<(RustConnection, usize, Atom, Atom)> {
        let (conn, screen) = x11rb::connect(None).ok()?;
        let active = conn
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")
            .ok()?
            .reply()
            .ok()?
            .atom;
        let pid = conn
            .intern_atom(false, b"_NET_WM_PID")
            .ok()?
            .reply()
            .ok()?
            .atom;
        Some((conn, screen, active, pid))
    }

    /// Executable base name of the focused window's process (lowercase),
    /// falling back to the WM_CLASS instance name.
    pub fn foreground_app(&mut self) -> Option<String> {
        if self.conn.is_none() {
            self.conn = Self::connect();
        }
        let r = self.query();
        if r.is_none() {
            // Maybe the connection died (X restart): retry fresh next time.
            if let Some((c, ..)) = &self.conn {
                if c.flush().is_err() {
                    self.conn = None;
                }
            }
        }
        r
    }

    fn query(&self) -> Option<String> {
        let (conn, screen, active, pid_atom) = self.conn.as_ref()?;
        let root = conn.setup().roots.get(*screen)?.root;
        let win = conn
            .get_property(false, root, *active, AtomEnum::WINDOW, 0, 1)
            .ok()?
            .reply()
            .ok()?
            .value32()?
            .next()?;
        if win == 0 {
            return None;
        }
        if let Some(name) = pid_name(conn, win, *pid_atom) {
            return Some(name);
        }
        let class = conn
            .get_property(false, win, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256)
            .ok()?
            .reply()
            .ok()?
            .value;
        let inst = class.split(|b| *b == 0).next()?;
        let s = String::from_utf8_lossy(inst).to_lowercase();
        (!s.is_empty()).then_some(s)
    }
}

fn pid_name(conn: &RustConnection, win: Window, pid_atom: Atom) -> Option<String> {
    let pid = conn
        .get_property(false, win, pid_atom, AtomEnum::CARDINAL, 0, 1)
        .ok()?
        .reply()
        .ok()?
        .value32()?
        .next()?;
    let exe = fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    let base = exe.file_name()?.to_string_lossy().to_lowercase();
    // "krita (deleted)" after an upgrade; interpreters (python, wine) are
    // still reported as-is, same as the process image name on Windows.
    let base = base.trim_end_matches(" (deleted)").to_string();
    (!base.is_empty()).then_some(base)
}

/// Grab the panic hotkey globally and call `on_panic` on every press.
/// Returns false immediately if there is no X server or the combo cannot be
/// grabbed (another app owns it); the thread is only started on success.
pub fn spawn_panic_hook(hotkey: &[String], on_panic: impl Fn() + Send + 'static) -> bool {
    let Some((main, mods)) = hotkey.split_last() else {
        return false;
    };
    let mut mask = 0u16;
    for m in mods {
        match mod_mask(m) {
            Some(b) => mask |= b,
            None => return false,
        }
    }
    let Some((_, evdev)) = chiz_core::profile::key_codes(main) else {
        return false;
    };
    // X keycodes are evdev codes + 8 on every mainstream xkb setup.
    let keycode = (evdev + 8) as u8;
    let Ok((conn, screen)) = x11rb::connect(None) else {
        return false;
    };
    let Some(root) = conn.setup().roots.get(screen).map(|s| s.root) else {
        return false;
    };
    for lock in LOCK_BITS {
        let ok = conn
            .grab_key(
                false,
                root,
                ModMask::from(mask | lock),
                keycode,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            )
            .map(|c| c.check().is_ok())
            .unwrap_or(false);
        if !ok {
            return false;
        }
    }
    if conn.flush().is_err() {
        return false;
    }
    std::thread::Builder::new()
        .name("chiz-panic-hotkey".into())
        .spawn(move || {
            while let Ok(ev) = conn.wait_for_event() {
                if let Event::KeyPress(k) = ev {
                    if k.detail == keycode {
                        on_panic();
                    }
                }
            }
            eprintln!("X connection lost: panic hotkey no longer active");
        })
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(s: &str) -> Vec<String> {
        s.split('+').map(String::from).collect()
    }

    #[test]
    fn modifier_masks_match_x11() {
        assert_eq!(mod_mask("shift"), Some(1));
        assert_eq!(mod_mask("ctrl"), Some(4));
        assert_eq!(mod_mask("alt"), Some(8));
        assert_eq!(mod_mask("meta"), Some(0x40));
        assert_eq!(mod_mask("f12"), None);
    }

    #[test]
    fn invalid_hotkeys_are_rejected_without_a_display() {
        assert!(!spawn_panic_hook(&[], || {}));
        assert!(!spawn_panic_hook(&keys("ctrl+bogus+f12"), || {}));
        assert!(!spawn_panic_hook(&keys("ctrl+nokey"), || {}));
    }

    #[test]
    fn foreground_probe_never_panics() {
        // With or without an X server this must just return Some/None.
        let mut p = ForegroundProbe::default();
        let _ = p.foreground_app();
        let _ = p.foreground_app();
    }
}
