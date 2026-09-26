//! OS injection adapter (spec 8/9/10): turns decoded pen records into real
//! OS input through the platform crates, applying mapping + pressure curve.
//! Sync std-mutex guarded so the panic thread can inject `leave` too.
//! When no device is available (missing /dev/uinput, no display), effects
//! are logged so sessions stay demonstrable end to end.

use chiz_core::{apply_pressure_curve, map_pen_to_px};

#[cfg(unix)]
use chiz_platform_linux::Backend;

#[derive(Clone, Debug)]
pub struct ViewConfig {
    pub union_w: i32,
    pub union_h: i32,
    pub area: (f64, f64, f64, f64),
    pub keep_aspect: bool,
    pub rotation: u16,
    pub curve: [f64; 4],
}

impl Default for ViewConfig {
    fn default() -> Self {
        Self {
            union_w: 1920,
            union_h: 1080,
            area: (0.0, 0.0, 1.0, 1.0),
            keep_aspect: true,
            rotation: 0,
            curve: [0.25, 0.25, 0.75, 0.75],
        }
    }
}

pub struct Injector {
    pub view: ViewConfig,
    #[cfg(unix)]
    pen: Option<chiz_platform_linux::PenDevice<chiz_platform_linux::FdBackend>>,
    #[cfg(unix)]
    keys: Option<chiz_platform_linux::FdBackend>,
    #[cfg(windows)]
    win: chiz_platform_windows::WindowsPlatform,
    #[cfg(unix)]
    last_ensure: f64,
}

impl Injector {
    pub fn new(view: ViewConfig) -> Self {
        Self {
            view,
            #[cfg(unix)]
            pen: None,
            #[cfg(unix)]
            keys: None,
            #[cfg(windows)]
            win: chiz_platform_windows::WindowsPlatform::default(),
            #[cfg(unix)]
            last_ensure: 0.0,
        }
    }

    pub fn set_view(&mut self, view: ViewConfig) {
        let changed = view.union_w != self.view.union_w || view.union_h != self.view.union_h;
        self.view = view;
        if changed {
            // Union resize recreates the Linux device, but only while the
            // pen is not touching (spec 10) — handled by dropping here and
            // recreating on next ensure() when idle.
            #[cfg(unix)]
            {
                self.pen = None;
            }
        }
    }

    /// Ensure devices exist (Linux creates uinput on demand; Windows lazily
    /// creates its synthetic device on first inject). Retried at most every
    /// 5 s so a missing /dev/uinput logs instead of spinning (spec 8).
    pub fn ensure(&mut self, now: f64) {
        #[cfg(unix)]
        {
            if self.pen.is_some() && self.keys.is_some() {
                return;
            }
            if now - self.last_ensure < 5.0 {
                return;
            }
            self.last_ensure = now;
            if self.keys.is_none() {
                match chiz_platform_linux::create_keys() {
                    Ok(k) => self.keys = Some(k),
                    Err(e) => eprintln!("keys device unavailable ({e}); will retry"),
                }
            }
            if self.pen.is_none() {
                match chiz_platform_linux::create_pen(self.view.union_w, self.view.union_h) {
                    Ok(p) => {
                        self.pen = Some(chiz_platform_linux::PenDevice::new(p, 0, 0));
                    }
                    Err(e) => eprintln!("pen device unavailable ({e}); will retry"),
                }
            }
        }
        #[cfg(windows)]
        {
            let _ = now;
        }
    }

    pub fn available(&self) -> bool {
        #[cfg(unix)]
        {
            self.pen.is_some()
        }
        #[cfg(windows)]
        {
            true
        }
        #[cfg(not(any(unix, windows)))]
        {
            false
        }
    }

    /// Map + curve + inject one decoded record. Raw record values in,
    /// OS events out; the eraser flag already includes eraser_toggle.
    #[allow(clippy::too_many_arguments)]
    pub fn pen(
        &mut self,
        phase_u8: u8,
        x: u16,
        y: u16,
        pressure: u16,
        distance: u16,
        tilt_x: i8,
        tilt_y: i8,
        eraser: bool,
        barrel1: bool,
        barrel2: bool,
        pen_aspect: (u32, u32),
    ) {
        let (pw, ph) = (pen_aspect.0.max(1) as f64, pen_aspect.1.max(1) as f64);
        let (mx, my) = map_pen_to_px(
            x as f64 / 65535.0,
            y as f64 / 65535.0,
            pw,
            ph,
            0.0,
            0.0,
            self.view.union_w as f64,
            self.view.union_h as f64,
            self.view.area,
            self.view.keep_aspect,
            self.view.rotation,
        );
        let p = apply_pressure_curve(&self.view.curve, pressure as f64 / 65535.0);
        let dist = distance as f64 / 65535.0;
        #[cfg(unix)]
        {
            if let Some(dev) = self.pen.as_mut() {
                use chiz_platform_linux::Phase as LP;
                let ph = match phase_u8 {
                    0 => LP::Hover,
                    1 => LP::Down,
                    2 => LP::Move,
                    3 => LP::Up,
                    4 => LP::Leave,
                    _ => LP::Leave, // cancel handled below as up+leave
                };
                if phase_u8 == 5 {
                    dev.record(LP::Up, mx as i32, my as i32, p, dist, tilt_x, tilt_y, eraser, barrel1, barrel2);
                }
                dev.record(ph, mx as i32, my as i32, p, dist, tilt_x, tilt_y, eraser, barrel1, barrel2);
                return;
            }
        }
        #[cfg(windows)]
        {
            use chiz_core::platform::{PenEvent, PenPhase};
            let phase = match phase_u8 {
                0 => PenPhase::Hover,
                1 => PenPhase::Down,
                2 => PenPhase::Move,
                3 => PenPhase::Up,
                4 | 5 => PenPhase::Leave,
                _ => PenPhase::Leave,
            };
            if phase_u8 == 5 {
                self.win.inject_pen(PenEvent {
                    phase: PenPhase::Up,
                    x_px: mx,
                    y_px: my,
                    pressure: p,
                    tilt_x,
                    tilt_y,
                    distance: dist,
                    eraser,
                    barrel1,
                    barrel2,
                });
            }
            self.win.inject_pen(PenEvent {
                phase,
                x_px: mx,
                y_px: my,
                pressure: p,
                tilt_x,
                tilt_y,
                distance: dist,
                eraser,
                barrel1,
                barrel2,
            });
            return;
        }
        // No device (or unsupported target): log so sessions stay visible.
        eprintln!(
            "inject_pen phase={phase_u8} x={mx} y={my} p={p:.3} tilt={tilt_x}/{tilt_y} eraser={eraser}"
        );
    }

    pub fn key(&mut self, key: &str, down: bool) {
        #[cfg(unix)]
        {
            if let Some(kb) = self.keys.as_mut() {
                let keys = vec![key.to_string()];
                chiz_platform_linux::emit_combo(kb, &keys, down);
                return;
            }
        }
        #[cfg(windows)]
        {
            self.win.inject_key(key, down);
            return;
        }
        eprintln!("inject_key {key} {}", if down { "down" } else { "up" });
    }

    /// Full combination press (tap/macros): ordered with SYN batching.
    pub fn combo(&mut self, keys: &[String], hold: bool) {
        #[cfg(unix)]
        {
            if let Some(kb) = self.keys.as_mut() {
                chiz_platform_linux::emit_combo(kb, keys, hold);
                return;
            }
        }
        #[cfg(windows)]
        {
            for (k, d) in chiz_core::profile::plan_key_press(keys, hold, false) {
                self.win.inject_key(&k, d);
            }
            return;
        }
        for (k, d) in chiz_core::profile::plan_key_press(keys, hold, false) {
            eprintln!("inject_key {k} {}", if d { "down" } else { "up" });
        }
    }

    pub fn key_release(&mut self, key: &str) {
        #[cfg(unix)]
        {
            if let Some(kb) = self.keys.as_mut() {
                use chiz_platform_linux::{Backend, EV_KEY, EV_SYN, SYN_REPORT};
                if let Some((_, ev)) = chiz_core::profile::key_codes(key) {
                    kb.emit(EV_KEY, ev, 0);
                    kb.emit(EV_SYN, SYN_REPORT, 0);
                    return;
                }
            }
        }
        #[cfg(windows)]
        {
            self.win.inject_key(key, false);
            return;
        }
        eprintln!("inject_key {key} up");
    }

    pub fn mouse(&mut self, button: &str, down: bool) {
        #[cfg(unix)]
        {
            if let Some(kb) = self.keys.as_mut() {
                use chiz_platform_linux::{BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, EV_KEY, EV_SYN, SYN_REPORT};
                let code = match button {
                    "left" => BTN_LEFT,
                    "right" => BTN_RIGHT,
                    "middle" => BTN_MIDDLE,
                    _ => return,
                };
                kb.emit(EV_KEY, code, down as i32);
                kb.emit(EV_SYN, SYN_REPORT, 0);
                return;
            }
        }
        #[cfg(windows)]
        {
            self.win.inject_mouse(button, down);
            return;
        }
        eprintln!("inject_mouse {button} {}", if down { "down" } else { "up" });
    }

    /// Pause gate / panic / watchdog: lift + leave, exactly once.
    pub fn force_leave(&mut self) {
        #[cfg(unix)]
        {
            if let Some(dev) = self.pen.as_mut() {
                dev.force_leave();
                return;
            }
        }
        #[cfg(windows)]
        {
            use chiz_core::platform::{PenEvent, PenPhase};
            if self.win.state.in_range {
                self.win.inject_pen(PenEvent {
                    phase: PenPhase::Leave,
                    x_px: 0,
                    y_px: 0,
                    pressure: 0.0,
                    tilt_x: 0,
                    tilt_y: 0,
                    distance: 0.0,
                    eraser: false,
                    barrel1: false,
                    barrel2: false,
                });
                return;
            }
        }
        eprintln!("inject_pen leave (no device)");
    }

    pub fn release_all(&mut self) {
        #[cfg(unix)]
        {
            // Held-key tracking lives in the action runner; the devices only
            // need pen lifted (keys are released as explicit ups).
            if let Some(dev) = self.pen.as_mut() {
                dev.force_leave();
            }
        }
        #[cfg(windows)]
        {
            self.win.release_all();
        }
    }
}
