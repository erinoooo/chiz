//! Linux platform layer (spec 10): two uinput devices, no kernel driver.
//!
//! Researched against kernel uinput docs + python-evdev patterns:
//! - "Chiz Pen": BUS_VIRTUAL, VID 0x4368 PID 0x0001, `INPUT_PROP_POINTER`
//!   (NOT `DIRECT`), EV_KEY+EV_ABS+EV_SYN; keys BTN_TOOL_PEN/RUBBER,
//!   BTN_TOUCH/STYLUS/STYLUS2; ABS_X/Y over union-size-1 (res 4/mm),
//!   PRESSURE 0..8191, DISTANCE 0..255, TILT_X/Y -90..90.
//!   Send `x - union_left`, `y - union_top`; every group ends SYN_REPORT.
//!   hover = tool-key-on + axes(PRESSURE 0); down = BTN_TOUCH 1 + axes;
//!   up = PRESSURE 0 + BTN_TOUCH 0; leave = tool-key-off. Tool switch =
//!   leave + hover in separate reports, only out of contact.
//! - "Chiz Keys": EV_KEY for every spec-11 key + BTN_LEFT/RIGHT/MIDDLE,
//!   EV_REL REL_X/REL_Y declared-but-unsent so it counts as a pointer.
//! - Access: udev rule `60-chiz-uinput.rules` + `modules-load.d/chiz.conf`;
//!   errors: missing node -> `sudo modprobe uinput`; EACCES -> udev steps.
//! - Monitors: X11 RandR `GetMonitors`; Wayland `wl_output` + xdg-output
//!   logical geometry. Foreground: X11 `_NET_ACTIVE_WINDOW` + `WM_CLASS[1]`
//!   lowercased; Wayland returns None (tray says auto-switch unavailable).
//! - Layout caveat: Linux key codes are positional (US layout); say so in
//!   the user guide. Compositor mapping table (X11/GNOME/KDE/Sway) must be
//!   verified in acceptance tests; workaround = output map to whole desktop.
//!
//! Panic hotkey thread (emergency off switch, X11):
//! - Dedicated `std::thread` (no tokio, no shared locks except the failsafe
//!   flag): `XOpenDisplay`, `XKeysymToKeycode(F12)`, `XGrabKey` on the root
//!   window for `Control|Mod1|Shift` (+ locked-modifier variants: NumLock,
//!   CapsLock via `XGetModifierMapping`), then an `XNextEvent` loop calling
//!   the panic entry on `KeyPress`.
//! - Must never allocate-then-block on the session tasks; on grab failure
//!   log the tray fallback ("Emergency release" item) and keep running.
//! - Wayland: no portable grab exists. Use xdg-desktop-portal
//!   `GlobalShortcuts` (RegisterSession + Bind) where available; otherwise
//!   the tray item is the panic switch and the UI says so.

// ---------------------------------------------------------------------------
// Event coding (linux/input.h + linux/uinput.h). Values are kernel ABI.

pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_REL: u16 = 0x02;
pub const EV_ABS: u16 = 0x03;
pub const SYN_REPORT: u16 = 0;

pub const BTN_TOOL_PEN: u16 = 0x140;
pub const BTN_TOOL_RUBBER: u16 = 0x141;
pub const BTN_TOUCH: u16 = 0x14a;
pub const BTN_STYLUS: u16 = 0x14b;
pub const BTN_STYLUS2: u16 = 0x14c;
pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_MIDDLE: u16 = 0x112;

pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;
pub const ABS_PRESSURE: u16 = 0x18;
pub const ABS_DISTANCE: u16 = 0x19;
pub const ABS_TILT_X: u16 = 0x1a;
pub const ABS_TILT_Y: u16 = 0x1b;
pub const REL_X: u16 = 0x00;
pub const REL_Y: u16 = 0x01;

pub const INPUT_PROP_POINTER: u16 = 0x01;
pub const BUS_VIRTUAL: u16 = 0x06;
pub const VID: u16 = 0x4368;
pub const PID: u16 = 0x0001;

pub const PRESSURE_MAX: i32 = 8191;
pub const DISTANCE_MAX: i32 = 255;

const UI_SET_EVBIT: u64 = 0x40045564;
const UI_SET_KEYBIT: u64 = 0x40045565;
const UI_SET_RELBIT: u64 = 0x40045566;
const UI_SET_ABSBIT: u64 = 0x40045567;
const UI_SET_PROPBIT: u64 = 0x4004556e;
const UI_DEV_SETUP: u64 = 0x405c8003;
const UI_DEV_CREATE: u64 = 0x5501;
const UI_DEV_DESTROY: u64 = 0x5502;

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct InputAbsinfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

#[repr(C)]
struct UinputSetup {
    id: InputId,
    name: [u8; 80],
    ff_effects_max: u32,
    absinfo: [InputAbsinfo; 64],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct InputEvent {
    sec: i64,
    usec: i64,
    typ: u16,
    code: u16,
    value: i32,
}

// ---------------------------------------------------------------------------
// Backend: real fd vs recording fake (tests + dry runs without /dev/uinput).

pub trait Backend {
    fn emit(&mut self, typ: u16, code: u16, value: i32);
    fn sync(&mut self) {
        self.emit(EV_SYN, SYN_REPORT, 0);
    }
}

/// Live kernel backend. Fails honestly when /dev/uinput is missing
/// ("Run: sudo modprobe uinput") or denied (udev rule steps).
pub struct FdBackend {
    fd: std::os::fd::RawFd,
}

impl FdBackend {
    fn ioctl(&self, req: u64, arg: i64) -> std::io::Result<()> {
        let r = unsafe { libc::ioctl(self.fd, req as _, arg) };
        if r < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Backend for FdBackend {
    fn emit(&mut self, typ: u16, code: u16, value: i32) {
        let ev = InputEvent {
            sec: 0,
            usec: 0,
            typ,
            code,
            value,
        };
        let bytes = unsafe {
            std::slice::from_raw_parts(
                (&raw const ev).cast::<u8>(),
                std::mem::size_of::<InputEvent>(),
            )
        };
        unsafe {
            libc::write(self.fd, bytes.as_ptr().cast(), bytes.len());
        }
    }
}

impl Drop for FdBackend {
    fn drop(&mut self) {
        unsafe {
            libc::ioctl(self.fd, UI_DEV_DESTROY as _);
            libc::close(self.fd);
        }
    }
}

/// Test/dry-run backend: records every event for exact-sequence asserts.
#[derive(Default)]
pub struct RecBackend {
    pub events: Vec<(u16, u16, i32)>,
}

impl Backend for RecBackend {
    fn emit(&mut self, typ: u16, code: u16, value: i32) {
        self.events.push((typ, code, value));
    }
}

// ---------------------------------------------------------------------------
// Device creation (real backend only).

fn open_uinput() -> std::io::Result<std::os::fd::RawFd> {
    let fd = unsafe {
        libc::open(
            c"/dev/uinput".as_ptr(),
            libc::O_WRONLY | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        let e = std::io::Error::last_os_error();
        if e.kind() == std::io::ErrorKind::NotFound {
            eprintln!("Run: sudo modprobe uinput");
        } else if e.kind() == std::io::ErrorKind::PermissionDenied {
            eprintln!("uinput denied: install 60-chiz-uinput.rules, reload udev, log out/in");
        }
        return Err(e);
    }
    Ok(fd)
}

fn setup_common(fd: std::os::fd::RawFd, name: &str, keys: &[u16], abs: &[(u16, i32, i32, i32)]) -> std::io::Result<()> {
    let io = |req: u64, arg: i64| -> std::io::Result<()> {
        if unsafe { libc::ioctl(fd, req as _, arg) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    };
    io(UI_SET_EVBIT, EV_SYN as i64)?;
    io(UI_SET_EVBIT, EV_KEY as i64)?;
    for k in keys {
        io(UI_SET_KEYBIT, *k as i64)?;
    }
    let mut setup = UinputSetup {
        id: InputId {
            bustype: BUS_VIRTUAL,
            vendor: VID,
            product: PID,
            version: 1,
        },
        name: [0; 80],
        ff_effects_max: 0,
        absinfo: [InputAbsinfo {
            value: 0,
            minimum: 0,
            maximum: 0,
            fuzz: 0,
            flat: 0,
            resolution: 0,
        }; 64],
    };
    let nb = name.as_bytes();
    setup.name[..nb.len().min(79)].copy_from_slice(&nb[..nb.len().min(79)]);
    if !abs.is_empty() {
        io(UI_SET_EVBIT, EV_ABS as i64)?;
        for (code, min, max, res) in abs {
            io(UI_SET_ABSBIT, *code as i64)?;
            setup.absinfo[*code as usize] = InputAbsinfo {
                value: 0,
                minimum: *min,
                maximum: *max,
                fuzz: 0,
                flat: 0,
                resolution: *res,
            };
        }
    }
    if unsafe { libc::ioctl(fd, UI_DEV_SETUP as _, &raw const setup) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Create the "Chiz Pen" device over `union_w` x `union_h` (compositor
/// logical space). Destroy + recreate when the union size changes, only
/// while the pen is not touching.
pub fn create_pen(union_w: i32, union_h: i32) -> std::io::Result<FdBackend> {
    let fd = open_uinput()?;
    setup_common(
        fd,
        "Chiz Pen",
        &[BTN_TOOL_PEN, BTN_TOOL_RUBBER, BTN_TOUCH, BTN_STYLUS, BTN_STYLUS2],
        &[
            (ABS_X, 0, union_w - 1, 4),
            (ABS_Y, 0, union_h - 1, 4),
            (ABS_PRESSURE, 0, PRESSURE_MAX, 0),
            (ABS_DISTANCE, 0, DISTANCE_MAX, 0),
            (ABS_TILT_X, -90, 90, 1),
            (ABS_TILT_Y, -90, 90, 1),
        ],
    )?;
    let back = FdBackend { fd };
    back.ioctl(UI_SET_PROPBIT, INPUT_PROP_POINTER as i64)?;
    if unsafe { libc::ioctl(fd, UI_DEV_CREATE as _) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(back)
}

/// Every key name the section-11 table knows, for EV_KEY declaration.
pub fn all_key_names() -> Vec<String> {
    let mut names = vec![
        "ctrl", "shift", "alt", "meta", "space", "enter", "tab", "esc", "backspace", "delete", "insert",
        "home", "end", "pageup", "pagedown", "left", "up", "right", "down", "minus", "equal",
        "bracketleft", "bracketright", "comma", "period", "slash", "backslash", "semicolon", "quote",
        "grave",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    for i in 1..=24 {
        names.push(format!("f{i}"));
    }
    for d in '0'..='9' {
        names.push(d.to_string());
    }
    for c in 'a'..='z' {
        names.push(c.to_string());
    }
    names.retain(|n| chiz_core::profile::key_codes(n).is_some());
    names
}

/// Create the "Chiz Keys" device (keys + mouse buttons; REL_X/Y declared
/// but never sent so it counts as a pointer too).
pub fn create_keys() -> std::io::Result<FdBackend> {
    let fd = open_uinput()?;
    let mut keys = vec![BTN_LEFT, BTN_RIGHT, BTN_MIDDLE];
    for n in all_key_names() {
        if let Some((_, ev)) = chiz_core::profile::key_codes(&n) {
            if !keys.contains(&ev) {
                keys.push(ev);
            }
        }
    }
    setup_common(fd, "Chiz Keys", &keys, &[])?;
    if unsafe { libc::ioctl(fd, UI_SET_EVBIT as _, EV_REL as i64) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::ioctl(fd, UI_SET_RELBIT as _, REL_X as i64) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::ioctl(fd, UI_SET_RELBIT as _, REL_Y as i64) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let back = FdBackend { fd };
    if unsafe { libc::ioctl(fd, UI_DEV_CREATE as _) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(back)
}

// ---------------------------------------------------------------------------
// Pen state machine: records -> exact kernel event groups (spec 10 table).

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Phase {
    Hover,
    Down,
    Move,
    Up,
    Leave,
}

pub struct PenDevice<B: Backend> {
    pub backend: B,
    in_range: bool,
    rubber: bool,
    touching: bool,
    barrel: (bool, bool),
    pub union_left: i32,
    pub union_top: i32,
}

impl<B: Backend> PenDevice<B> {
    pub fn new(backend: B, union_left: i32, union_top: i32) -> Self {
        Self {
            backend,
            in_range: false,
            rubber: false,
            touching: false,
            barrel: (false, false),
            union_left,
            union_top,
        }
    }

    fn tool(&self) -> u16 {
        if self.rubber {
            BTN_TOOL_RUBBER
        } else {
            BTN_TOOL_PEN
        }
    }

    fn axes(&mut self, x: i32, y: i32, pressure01: f64, distance01: f64, tilt_x: i8, tilt_y: i8, touch_pressure: bool) {
        let p = if touch_pressure {
            (pressure01 * PRESSURE_MAX as f64).round() as i32
        } else {
            0
        };
        let d = (distance01 * DISTANCE_MAX as f64).round() as i32;
        self.backend.emit(EV_ABS, ABS_X, x - self.union_left);
        self.backend.emit(EV_ABS, ABS_Y, y - self.union_top);
        self.backend.emit(EV_ABS, ABS_PRESSURE, p);
        self.backend.emit(EV_ABS, ABS_DISTANCE, d);
        self.backend.emit(EV_ABS, ABS_TILT_X, tilt_x as i32);
        self.backend.emit(EV_ABS, ABS_TILT_Y, tilt_y as i32);
    }

    fn barrel_to(&mut self, b1: bool, b2: bool) {
        // Events only on change (spec 10).
        if b1 != self.barrel.0 {
            self.backend.emit(EV_KEY, BTN_STYLUS, b1 as i32);
            self.barrel.0 = b1;
        }
        if b2 != self.barrel.1 {
            self.backend.emit(EV_KEY, BTN_STYLUS2, b2 as i32);
            self.barrel.1 = b2;
        }
    }

    /// One decoded pen record. Coordinates are compositor pixels.
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &mut self,
        phase: Phase,
        x: i32,
        y: i32,
        pressure01: f64,
        distance01: f64,
        tilt_x: i8,
        tilt_y: i8,
        eraser: bool,
        barrel1: bool,
        barrel2: bool,
    ) {
        match phase {
            Phase::Hover => {
                if self.touching {
                    self.up();
                }
                if !self.in_range || eraser != self.rubber {
                    if self.in_range {
                        self.backend.emit(EV_KEY, self.tool(), 0);
                        self.backend.sync(); // tool switch: separate reports
                    }
                    self.rubber = eraser;
                    self.backend.emit(EV_KEY, self.tool(), 1);
                    self.in_range = true;
                }
                self.axes(x, y, pressure01, distance01, tilt_x, tilt_y, false);
                self.barrel_to(barrel1, barrel2);
                self.backend.sync();
            }
            Phase::Down | Phase::Move => {
                if !self.in_range {
                    self.rubber = eraser;
                    self.backend.emit(EV_KEY, self.tool(), 1);
                    self.in_range = true;
                }
                if !self.touching {
                    self.backend.emit(EV_KEY, BTN_TOUCH, 1);
                    self.touching = true;
                }
                self.axes(x, y, pressure01, distance01, tilt_x, tilt_y, true);
                self.barrel_to(barrel1, barrel2);
                self.backend.sync();
            }
            Phase::Up => self.up(),
            Phase::Leave => {
                if self.touching {
                    self.up();
                }
                if self.in_range {
                    self.backend.emit(EV_KEY, self.tool(), 0);
                    self.backend.sync();
                    self.in_range = false;
                }
            }
        }
    }

    fn up(&mut self) {
        if !self.touching {
            return;
        }
        self.backend.emit(EV_ABS, ABS_PRESSURE, 0);
        self.backend.emit(EV_KEY, BTN_TOUCH, 0);
        self.backend.sync();
        self.touching = false;
    }

    /// Pause gate / failure banner path: lift + leave, exactly once.
    pub fn force_leave(&mut self) {
        if self.touching {
            self.up();
        }
        if self.in_range {
            self.backend.emit(EV_KEY, self.tool(), 0);
            self.backend.sync();
            self.in_range = false;
        }
    }
}

// ---------------------------------------------------------------------------
// Keys + mouse (spec 10): each key event followed by SYN_REPORT, ~2 ms apart.

/// Emit one parsed combo: modifiers down, key down/up, modifiers up reverse.
/// Hold callers pass `hold=true` to keep keys down (released separately).
pub fn emit_combo<B: Backend>(backend: &mut B, keys: &[String], hold: bool) {
    let mods: Vec<&String> = keys[..keys.len().saturating_sub(1)]
        .iter()
        .filter(|k| chiz_core::profile::is_modifier(k))
        .collect();
    let main = keys.last().cloned().unwrap_or_default();
    let code = |n: &str| chiz_core::profile::key_codes(n).map(|(_, ev)| ev).unwrap_or(0);
    for m in &mods {
        backend.emit(EV_KEY, code(m), 1);
        backend.sync();
    }
    if chiz_core::profile::is_modifier(&main) && keys.len() == 1 {
        backend.emit(EV_KEY, code(&main), 1);
        backend.sync();
        if !hold {
            backend.emit(EV_KEY, code(&main), 0);
            backend.sync();
        }
    } else if !chiz_core::profile::is_modifier(&main) {
        backend.emit(EV_KEY, code(&main), 1);
        backend.sync();
        if !hold {
            backend.emit(EV_KEY, code(&main), 0);
            backend.sync();
        }
    }
    if !hold {
        for m in mods.iter().rev() {
            backend.emit(EV_KEY, code(m), 0);
            backend.sync();
        }
    }
}

/// X11 panic-hook entry. Real `XGrabKey` loop lands here (needs an X
/// display at runtime); until then this documents the contract: `on_panic`
/// runs on its own OS thread and must only flip the failsafe + release.
pub fn spawn_panic_hook(_on_panic: impl Fn() + Send + 'static) {
    // TODO(M7): XOpenDisplay/XGrabKey/XNextEvent loop on a std::thread.
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev() -> PenDevice<RecBackend> {
        PenDevice::new(RecBackend::default(), 0, 0)
    }

    fn syns(ev: &[(u16, u16, i32)]) -> usize {
        ev.iter().filter(|(t, c, _)| *t == EV_SYN && *c == SYN_REPORT).count()
    }

    #[test]
    fn hover_down_move_up_leave_sequence() {
        let mut d = dev();
        d.record(Phase::Hover, 100, 200, 0.0, 0.5, 0, 0, false, false, false);
        assert!(d.backend.events.contains(&(EV_KEY, BTN_TOOL_PEN, 1)));
        assert!(d.backend.events.contains(&(EV_ABS, ABS_PRESSURE, 0)));
        d.backend.events.clear();
        d.record(Phase::Down, 100, 200, 0.5, 0.0, 0, 0, false, false, false);
        assert!(d.backend.events.contains(&(EV_KEY, BTN_TOUCH, 1)));
        assert!(d.backend.events.contains(&(EV_ABS, ABS_PRESSURE, 4096))); // round(.5*8191)
        d.backend.events.clear();
        d.record(Phase::Move, 110, 210, 1.0, 0.0, 10, -5, false, true, false);
        assert!(d.backend.events.contains(&(EV_ABS, ABS_PRESSURE, 8191)));
        assert!(d.backend.events.contains(&(EV_ABS, ABS_TILT_X, 10)));
        assert!(d.backend.events.contains(&(EV_KEY, BTN_STYLUS, 1)));
        d.record(Phase::Up, 110, 210, 0.0, 0.0, 0, 0, false, true, false);
        assert!(d.backend.events.contains(&(EV_KEY, BTN_TOUCH, 0)));
        d.record(Phase::Leave, 0, 0, 0.0, 0.0, 0, 0, false, false, false);
        assert!(d.backend.events.contains(&(EV_KEY, BTN_TOOL_PEN, 0)));
        // Move, Up, Leave each end a report with SYN (3 since the clear).
        assert!(syns(&d.backend.events) >= 3);
    }

    #[test]
    fn eraser_tool_switch_is_leave_then_hover() {
        let mut d = dev();
        d.record(Phase::Hover, 50, 50, 0.0, 0.3, 0, 0, false, false, false);
        d.backend.events.clear();
        d.record(Phase::Hover, 50, 50, 0.0, 0.3, 0, 0, true, false, false);
        let ev = &d.backend.events;
        let off = ev.iter().position(|e| *e == (EV_KEY, BTN_TOOL_PEN, 0)).unwrap();
        let on = ev.iter().position(|e| *e == (EV_KEY, BTN_TOOL_RUBBER, 1)).unwrap();
        assert!(off < on); // separate reports, leave before hover
    }

    #[test]
    fn duplicate_up_is_ignored() {
        let mut d = dev();
        d.record(Phase::Up, 0, 0, 0.0, 0.0, 0, 0, false, false, false);
        assert!(d.backend.events.is_empty());
    }

    #[test]
    fn combo_order_with_syn_each() {
        let mut b = RecBackend::default();
        emit_combo(&mut b, &["ctrl".into(), "z".into()], false);
        let keys: Vec<(u16, i32)> = b
            .events
            .iter()
            .filter(|(t, _, _)| *t == EV_KEY)
            .map(|(_, c, v)| (*c, *v))
            .collect();
        assert_eq!(keys, vec![(29, 1), (44, 1), (44, 0), (29, 0)]);
        // Every key event followed by SYN_REPORT.
        assert_eq!(syns(&b.events), 4);
    }

    #[test]
    fn key_table_covers_profiles() {
        // Every key used by shipped profiles must exist on the Keys device.
        for n in ["ctrl", "shift", "z", "b", "space", "equal", "minus", "s"] {
            assert!(chiz_core::profile::key_codes(n).is_some(), "{n}");
        }
        assert!(!all_key_names().is_empty());
    }
}
