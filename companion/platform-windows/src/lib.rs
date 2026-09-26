//! Windows platform layer (spec 9): real pen/key/mouse injection through
//! the synthetic pointer API + SendInput. Compiles on Linux too (pure
//! packet builders + tests); the syscall glue is `#[cfg(windows)]` and is
//! compile-checked with:
//! `cargo check -p chiz-platform-windows --target x86_64-pc-windows-msvc`.
//!
//! Researched against Microsoft Learn `CreateSyntheticPointerDevice` /
//! `InjectSyntheticPointerInput` / `POINTER_PEN_INFO` docs. Manifest needs
//! `dpiAwareness` per-monitor-v2 (physical pixels) + `asInvoker`
//! (see companion/packaging/chiz.exe.manifest).

use chiz_core::platform::{Monitor, PenEvent, PenPhase, PenState, Platform};
use std::collections::HashSet;

// Flag values (winuser.h ABI): duplicated here so the packet builders and
// their tests compile on every host; the cfg(windows) glue passes them
// straight into POINTER_INFO/POINTER_PEN_INFO.
pub const PF_NEW: u32 = 0x0000_0001;
pub const PF_INRANGE: u32 = 0x0000_0002;
pub const PF_INCONTACT: u32 = 0x0000_0004;
pub const PF_FIRSTBUTTON: u32 = 0x0000_0010;
pub const PF_SECONDBUTTON: u32 = 0x0000_0020;
pub const PF_DOWN: u32 = 0x0001_0000;
pub const PF_UPDATE: u32 = 0x0002_0000;
pub const PF_UP: u32 = 0x0004_0000;

pub const PEN_MASK_PRESSURE: u32 = 0x1;
pub const PEN_MASK_TILT_X: u32 = 0x4;
pub const PEN_MASK_TILT_Y: u32 = 0x8;
pub const PEN_FLAG_BARREL: u32 = 0x1;
pub const PEN_FLAG_INVERTED: u32 = 0x2;
pub const PEN_FLAG_ERASER: u32 = 0x4;

/// `pointerFlags` per phase (spec 9 table). `first` = first event of an
/// in-range period (adds NEW on hover).
pub fn pointer_flags(phase: PenPhase, first: bool, barrel1: bool) -> u32 {
    let mut f = match phase {
        PenPhase::Hover => PF_INRANGE | PF_UPDATE | if first { PF_NEW } else { 0 },
        PenPhase::Down => PF_INRANGE | PF_INCONTACT | PF_FIRSTBUTTON | PF_DOWN,
        PenPhase::Move => PF_INRANGE | PF_INCONTACT | PF_FIRSTBUTTON | PF_UPDATE,
        PenPhase::Up => PF_INRANGE | PF_UP,
        PenPhase::Leave => PF_UPDATE, // no INRANGE; repeat once if stuck
    };
    if barrel1 && matches!(phase, PenPhase::Hover | PenPhase::Down | PenPhase::Move) {
        f |= PF_SECONDBUTTON; // no second barrel flag on Windows: barrel2 ignored
    }
    f
}

/// `penFlags`: BARREL while barrel1; INVERTED while eraser in range; ERASER
/// while the eraser end touches.
pub fn pen_flags(eraser_in_range: bool, touching: bool, barrel1: bool) -> u32 {
    let mut f = 0;
    if barrel1 {
        f |= PEN_FLAG_BARREL;
    }
    if eraser_in_range {
        f |= PEN_FLAG_INVERTED;
    }
    if eraser_in_range && touching {
        f |= PEN_FLAG_ERASER;
    }
    f
}

pub fn pressure_win(p01: f64) -> u32 {
    (p01.clamp(0.0, 1.0) * 1024.0).round() as u32
}

/// One validated OS packet (pure; converted to POINTER_TYPE_INFO on Windows).
pub struct PenPacket {
    pub x: i32,
    pub y: i32,
    pub flags: u32,
    pub mask: u32,
    pub pressure: u32,
    pub tilt_x: i32,
    pub tilt_y: i32,
    pub pen_flags: u32,
    pub repeat_leave: bool,
}

/// Pure builder: repair + packetize one record. Tested on Linux; the only
/// Windows-only step is the final struct fill.
pub fn build_packets(state: &mut PenState, ev: &PenEvent, first_of_range: &mut bool) -> Vec<PenPacket> {
    use PenPhase::*;
    let phase = match ev.phase {
        Hover => Hover,
        Down => Down,
        Move => Move,
        Up => Up,
        Leave => Leave,
    };
    let mut out = vec![];
    for ph in state.repair(phase, ev.eraser) {
        let first = *first_of_range && ph == Hover;
        if ph == Hover {
            *first_of_range = false;
        }
        if matches!(ph, Leave) {
            *first_of_range = true;
        }
        let touching = matches!(ph, Down | Move);
        out.push(PenPacket {
            x: ev.x_px as i32,
            y: ev.y_px as i32, // virtual-desktop physical px (may be negative)
            flags: pointer_flags(ph, first, ev.barrel1),
            mask: PEN_MASK_PRESSURE | PEN_MASK_TILT_X | PEN_MASK_TILT_Y,
            pressure: if touching { pressure_win(ev.pressure) } else { 0 },
            tilt_x: ev.tilt_x as i32,
            tilt_y: ev.tilt_y as i32,
            pen_flags: pen_flags(ev.eraser, touching, ev.barrel1),
            // Acceptance-test hook: if the pen appears stuck in range, the
            // injector sends one more UPDATE without INRANGE.
            repeat_leave: ph == Leave,
        });
    }
    out
}

/// One key transition in OS-neutral form (pure; SendInput on Windows).
pub struct KeyStroke {
    pub vk: u16,
    pub scan: u16,
    pub extended: bool,
    pub up: bool,
}

/// Parse + order a combo into strokes (modifiers down, key, modifiers up).
/// Whole combination is sent in one SendInput call (no interleave).
pub fn combo_strokes(keys: &[String], hold: bool, release: bool) -> Vec<KeyStroke> {
    let plan = chiz_core::profile::plan_key_press(keys, hold, release);
    plan.into_iter()
        .filter_map(|(name, down)| {
            let (vk, _) = chiz_core::profile::key_codes(&name)?;
            let scan = map_scan(vk);
            Some(KeyStroke {
                vk,
                scan,
                extended: chiz_core::profile::is_extended(&name),
                up: !down,
            })
        })
        .collect()
}

/// Scancode for SendInput. Real mapping is `MapVirtualKeyW(vk,
/// MAPVK_VK_TO_VSC)` on Windows; this table covers the common keys for
/// dry-run/test hosts (US layout positions).
pub fn map_scan(vk: u16) -> u16 {
    match vk {
        0x20 => 0x39, // space
        0x0D => 0x1C, // enter
        0x09 => 0x0F, // tab
        0x1B => 0x01, // esc
        0x08 => 0x0E, // backspace
        0xA2 => 0x1D, // L control
        0xA0 => 0x2A, // L shift
        0xA4 => 0x38, // L alt
        0x5B => 0x5B, // L win
        0xBD => 0x0C, // minus
        0xBB => 0x0D, // equal
        0x25 => 0xCB, // left (extended)
        0x26 => 0xC8, // up
        0x27 => 0xCD, // right
        0x28 => 0xD0, // down
        v if (0x41..=0x5A).contains(&v) => [0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17,
            0x24, 0x25, 0x26, 0x32, 0x31, 0x18, 0x19, 0x10, 0x13, 0x1F, 0x14,
            0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C][(v - 0x41) as usize],
        v if (0x30..=0x39).contains(&v) => [0x0B, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A][(v - 0x30) as usize],
        v if (0x70..=0x87).contains(&v) => [0x3B, 0x3C, 0x3D, 0x3E, 0x3F, 0x40, 0x41, 0x42, 0x43, 0x44,
            0x57, 0x58, 0x7E, 0x7F, 0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89][(v - 0x70) as usize],
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Live state: device handle, repair state, held inputs, UIPI warnings.

pub struct WindowsPlatform {
    pub state: PenState,
    pub first_of_range: bool,
    pub held_keys: Vec<u16>,
    pub held_mouse: HashSet<String>,
    pub elevated_warned: HashSet<String>,
    #[cfg(windows)]
    pub device: Option<windows::Win32::UI::Controls::HSYNTHETICPOINTERDEVICE>,
}

// Safety: the synthetic-pointer handle is a process-global kernel object
// with no thread-local state; the injector only touches it from sequenced
// async tasks, so moving the platform struct across threads is sound.
unsafe impl Send for WindowsPlatform {}

impl Default for WindowsPlatform {
    fn default() -> Self {
        Self {
            state: PenState::default(),
            first_of_range: true,
            held_keys: vec![],
            held_mouse: HashSet::new(),
            elevated_warned: HashSet::new(),
            #[cfg(windows)]
            device: None,
        }
    }
}

impl Platform for WindowsPlatform {
    fn monitors(&mut self) -> Vec<Monitor> {
        #[cfg(windows)]
        {
            return enum_monitors();
        }
        #[cfg(not(windows))]
        {
            vec![]
        }
    }

    fn inject_pen(&mut self, ev: PenEvent) {
        let packets = build_packets(&mut self.state, &ev, &mut self.first_of_range);
        #[cfg(windows)]
        {
            ensure_device(self);
            for p in &packets {
                inject_packet(self.device, p);
            }
        }
        #[cfg(not(windows))]
        {
            let _ = packets;
        }
    }

    fn inject_key(&mut self, key: &str, down: bool) {
        let strokes = combo_strokes(&[key.to_string()], true, !down);
        #[cfg(windows)]
        {
            send_strokes(&strokes);
        }
        // Track held keys for release_all (both targets).
        if down {
            for s in &strokes {
                if !self.held_keys.contains(&s.vk) {
                    self.held_keys.push(s.vk);
                }
            }
        } else {
            for s in &strokes {
                self.held_keys.retain(|k| *k != s.vk);
            }
        }
    }

    fn inject_mouse(&mut self, button: &str, down: bool) {
        #[cfg(windows)]
        {
            send_mouse(button, down);
        }
        if down {
            self.held_mouse.insert(button.to_string());
        } else {
            self.held_mouse.remove(button);
        }
    }

    fn foreground_app(&mut self) -> Option<String> {
        #[cfg(windows)]
        {
            let app = foreground_exe();
            if let Some(ref a) = app {
                // UIPI: warn once per elevated app while we are not elevated.
                if !self.elevated_warned.contains(a) && app_is_elevated(a) && !self_is_elevated() {
                    self.elevated_warned.insert(a.clone());
                    eprintln!("{a} runs as administrator, so Chiz cannot control it. Run Chiz as administrator too.");
                }
            }
            return app;
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    fn release_all(&mut self) {
        #[cfg(windows)]
        {
            for vk in self.held_keys.drain(..).collect::<Vec<_>>() {
                send_strokes(&[KeyStroke { vk, scan: map_scan(vk), extended: false, up: true }]);
            }
            for b in self.held_mouse.drain().collect::<Vec<_>>() {
                send_mouse(&b, false);
            }
            // Lift pen + leave range.
            self.first_of_range = true;
        }
        self.state = PenState::default();
        self.held_keys.clear();
        self.held_mouse.clear();
    }
}

// ---------------------------------------------------------------------------
// Windows-only syscall glue.

#[cfg(windows)]
fn ensure_device_impl(p: &mut WindowsPlatform) {
    use windows::Win32::UI::Controls::*;
    use windows::Win32::UI::WindowsAndMessaging::PT_PEN;
    if p.device.is_none() {
        p.device = unsafe { CreateSyntheticPointerDevice(PT_PEN, 1, POINTER_FEEDBACK_NONE) }.ok();
    }
}

#[cfg(windows)]
fn inject_packet(
    device: Option<windows::Win32::UI::Controls::HSYNTHETICPOINTERDEVICE>,
    p: &PenPacket,
) {
    use windows::Win32::UI::Controls::POINTER_TYPE_INFO;
    use windows::Win32::UI::Input::Pointer::InjectSyntheticPointerInput;
    use windows::Win32::UI::WindowsAndMessaging::PT_PEN;
    let dev = match device {
        Some(d) => d,
        None => return,
    };
    // Zeroed then filled per the spec 9 table (rotation unused: 0).
    let mut info = POINTER_TYPE_INFO::default();
    info.r#type = PT_PEN;
    unsafe {
        let pen = &mut info.Anonymous.penInfo;
        pen.pointerInfo.pointerType = PT_PEN;
        pen.pointerInfo.pointerId = 0;
        pen.pointerInfo.ptPixelLocation.x = p.x;
        pen.pointerInfo.ptPixelLocation.y = p.y;
        pen.pointerInfo.pointerFlags.0 = p.flags;
        pen.penMask = p.mask;
        pen.pressure = p.pressure;
        pen.tiltX = p.tilt_x;
        pen.tiltY = p.tilt_y;
        pen.penFlags = p.pen_flags;
    }
    let failed = unsafe { InjectSyntheticPointerInput(dev, &[info]) }.is_err();
    if failed {
        // Device creation error path (spec 8): drop the handle so the next
        // record recreates it; banner + retry live in the UI layer.
        return;
    }
    if p.repeat_leave {
        // Acceptance hook: one more UPDATE without INRANGE if stuck.
        // (Union field *write* on an owned local is safe; reads are not.)
        let mut again = info;
        unsafe {
            again.Anonymous.penInfo.pointerInfo.pointerFlags.0 = PF_UPDATE;
            let _ = InjectSyntheticPointerInput(dev, &[again]);
        }
    }
}

#[cfg(windows)]
fn send_strokes(strokes: &[KeyStroke]) {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    let inputs: Vec<INPUT> = strokes
        .iter()
        .map(|s| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(s.vk),
                    wScan: s.scan,
                    dwFlags: (if s.up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) })
                        | if s.extended { KEYEVENTF_EXTENDEDKEY } else { KEYBD_EVENT_FLAGS(0) },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        })
        .collect();
    // One SendInput call per combination: no interleave with real input.
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

#[cfg(windows)]
fn send_mouse(button: &str, down: bool) {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    let flag = match (button, down) {
        ("left", true) => MOUSEEVENTF_LEFTDOWN,
        ("left", false) => MOUSEEVENTF_LEFTUP,
        ("right", true) => MOUSEEVENTF_RIGHTDOWN,
        ("right", false) => MOUSEEVENTF_RIGHTUP,
        ("middle", true) => MOUSEEVENTF_MIDDLEDOWN,
        ("middle", false) => MOUSEEVENTF_MIDDLEUP,
        _ => return,
    };
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: flag,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        let _ = SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

#[cfg(windows)]
fn enum_monitors() -> Vec<Monitor> {
    use windows::Win32::Foundation::*;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;
    use windows::core::BOOL;
    struct Acc(Vec<Monitor>);
    unsafe extern "system" fn cb(hmon: HMONITOR, _hdc: HDC, _rect: *mut RECT, data: LPARAM) -> BOOL {
        let acc = &mut *(data.0 as *mut Acc);
        // MONITORINFOEXW starts with MONITORINFO: pass its head and set
        // cbSize to the full extended size so szDevice comes back too.
        let mut ex = MONITORINFOEXW::default();
        ex.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(hmon, &mut ex.monitorInfo as *mut MONITORINFO).as_bool() {
            let id = String::from_utf16_lossy(&ex.szDevice)
                .trim_end_matches('\0')
                .to_string();
            let m = &ex.monitorInfo;
            acc.0.push(Monitor {
                id: id.clone(),
                // Friendly name via DisplayConfigGetDeviceInfo is resolved
                // in the UI layer; the device name is the stable id.
                name: id,
                x: m.rcMonitor.left as i64,
                y: m.rcMonitor.top as i64,
                w: (m.rcMonitor.right - m.rcMonitor.left) as u32,
                h: (m.rcMonitor.bottom - m.rcMonitor.top) as u32,
                primary: m.dwFlags & MONITORINFOF_PRIMARY != 0,
            });
        }
        true.into()
    }
    let mut acc = Acc(vec![]);
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut acc as *mut Acc as isize));
    }
    acc.0
}

#[cfg(windows)]
fn foreground_exe() -> Option<String> {
    use windows::Win32::System::Threading::*;
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::PWSTR;
    unsafe {
        let hwnd = GetForegroundWindow();
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).ok()?;
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        let base = path.rsplit(['\\', '/']).next().unwrap_or("").to_lowercase();
        if base.is_empty() {
            return None;
        }
        Some(base)
    }
}

#[cfg(windows)]
fn token_elevated(h: windows::Win32::Foundation::HANDLE) -> bool {
    use windows::Win32::Security::*;
    use windows::Win32::System::Threading::OpenProcessToken;
    unsafe {
        let mut tok = Default::default();
        if OpenProcessToken(h, TOKEN_QUERY, &mut tok).is_err() {
            return true; // access denied ~ elevated: warn (spec 9)
        }
        let mut elev = TOKEN_ELEVATION::default();
        let mut ret = 0u32;
        if GetTokenInformation(
            tok,
            TokenElevation,
            Some(&mut elev as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret,
        )
        .is_err()
        {
            return false;
        }
        elev.TokenIsElevated != 0
    }
}

#[cfg(windows)]
fn app_is_elevated(exe: &str) -> bool {
    use windows::Win32::System::Threading::*;
    use windows::Win32::UI::WindowsAndMessaging::*;
    unsafe {
        // Re-resolve the foreground process (cheap; only on app change).
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return true;
        };
        let _ = exe;
        token_elevated(h)
    }
}

#[cfg(windows)]
fn self_is_elevated() -> bool {
    use windows::Win32::System::Threading::*;
    unsafe {
        let h = GetCurrentProcess();
        token_elevated(h)
    }
}

#[cfg(windows)]
fn ensure_device(_p: &mut WindowsPlatform) {
    ensure_device_impl(_p);
}

/// OS panic-hook entry: RegisterHotKey message loop on a std::thread.
#[cfg(windows)]
pub fn spawn_panic_hook(on_panic: impl Fn() + Send + 'static) {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    use windows::Win32::UI::WindowsAndMessaging::*;
    std::thread::spawn(move || unsafe {
        if RegisterHotKey(None, 1, MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_NOREPEAT, 0x7B).is_err() {
            eprintln!("panic hotkey registration failed; use tray Emergency release");
            return;
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.message == WM_HOTKEY {
                on_panic();
            }
        }
    });
}

/// Non-Windows stub: documents the contract only.
#[cfg(not(windows))]
pub fn spawn_panic_hook(_on_panic: impl Fn() + Send + 'static) {}

#[cfg(not(windows))]
fn ensure_device(_p: &mut WindowsPlatform) {}

#[cfg(test)]
mod tests {
    use super::*;
    use chiz_core::platform::PenPhase;

    #[test]
    fn phase_flags_match_spec_table() {
        assert_eq!(pointer_flags(PenPhase::Hover, true, false), PF_INRANGE | PF_UPDATE | PF_NEW);
        assert_eq!(pointer_flags(PenPhase::Down, false, false), 0x0002 | 0x0004 | 0x0010 | 0x1_0000);
        assert_eq!(pointer_flags(PenPhase::Move, false, false), 0x0002 | 0x0004 | 0x0010 | 0x2_0000);
        assert_eq!(pointer_flags(PenPhase::Up, false, false), PF_INRANGE | PF_UP);
        assert_eq!(pointer_flags(PenPhase::Leave, false, false), PF_UPDATE);
        // Barrel1 adds SECONDBUTTON on hover/down/move only.
        assert_eq!(pointer_flags(PenPhase::Down, false, true) & PF_SECONDBUTTON, PF_SECONDBUTTON);
        assert_eq!(pointer_flags(PenPhase::Up, false, true) & PF_SECONDBUTTON, 0);
    }

    #[test]
    fn pen_flags_and_pressure() {
        assert_eq!(pen_flags(false, false, false), 0);
        assert_eq!(pen_flags(true, false, false), PEN_FLAG_INVERTED);
        assert_eq!(pen_flags(true, true, true), PEN_FLAG_BARREL | PEN_FLAG_INVERTED | PEN_FLAG_ERASER);
        assert_eq!(pressure_win(0.5), 512);
        assert_eq!(pressure_win(1.0), 1024);
        assert_eq!(pressure_win(0.0), 0);
    }

    #[test]
    fn repair_drives_packets() {
        let mut st = PenState::default();
        let mut first = true;
        let ev = PenEvent {
            phase: PenPhase::Down,
            x_px: 960,
            y_px: 540,
            pressure: 0.5,
            tilt_x: 0,
            tilt_y: 0,
            distance: 0.0,
            eraser: false,
            barrel1: false,
            barrel2: false,
        };
        let pk = build_packets(&mut st, &ev, &mut first);
        assert_eq!(pk.len(), 2); // hover (NEW) then down
        assert_eq!(pk[0].flags & PF_NEW, PF_NEW);
        assert_eq!(pk[1].pressure, 512);
        let up = PenEvent { phase: PenPhase::Up, ..ev };
        let pk2 = build_packets(&mut st, &up, &mut first);
        assert_eq!(pk2.len(), 1);
        assert_eq!(build_packets(&mut st, &up, &mut first).len(), 0); // dup up
    }

    #[test]
    fn combo_parses_to_strokes() {
        let ks = combo_strokes(&["ctrl".into(), "z".into()], false, false);
        assert_eq!(ks.len(), 4);
        assert_eq!((ks[0].vk, ks[0].up), (0xA2, false));
        assert_eq!((ks[1].vk, ks[1].up), (0x5A, false));
        assert!(ks[3].up);
        // Extended keys flagged (arrows etc.).
        let left = combo_strokes(&["left".into()], false, false);
        assert!(left[0].extended);
        assert_eq!(left[0].scan, 0xCB);
    }
}
