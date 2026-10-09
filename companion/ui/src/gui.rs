//! Native GUI runner (eframe): launching the companion opens the main
//! window by default. `--headless`, `CHIZ_HEADLESS=1`, or no display server
//! runs server-only (logs + mDNS + tablet only).
//!
//! The screens in `screens.rs` stay I/O-free: each frame copies a snapshot
//! out of `App` (short std-mutex reads), renders, then runs the
//! [PendingAction](super::screens::PendingAction)s the buttons queued. Every
//! action is a plain synchronous `App` call: the GUI thread never waits on
//! the async runtime.

use std::sync::Arc;
use std::time::Duration;

use super::app::App;
use super::screens::{CompanionApp, PendingAction};
use super::util::{now_secs, LockExt};
use chiz_core::failsafe::PauseReason;

pub struct GuiApp {
    core: Arc<App>,
    app: CompanionApp,
    qr_tex: Option<egui::TextureHandle>,
    qr_mtime: Option<std::time::SystemTime>,
    pushed_view: Option<ViewKey>,
    hosts_at: Option<std::time::Instant>,
    #[cfg(debug_assertions)]
    shot_frame: u32,
}

type ViewKey = ((f64, f64, f64, f64), bool, u16, [f64; 4]);

impl GuiApp {
    pub fn new(core: Arc<App>) -> Self {
        let mut me = Self {
            core,
            app: CompanionApp::default(),
            qr_tex: None,
            qr_mtime: None,
            pushed_view: None,
            hosts_at: None,
            #[cfg(debug_assertions)]
            shot_frame: 0,
        };
        me.seed_view();
        me
    }

    /// Load the freshly rendered pairing QR into a retained texture.
    /// Reloads only when the file changes; dropped when the window closes.
    fn sync_qr(&mut self, ctx: &egui::Context) {
        if !self.app.tablets.pairing_open {
            self.qr_tex = None;
            self.qr_mtime = None;
            self.app.tablets.qr_tex = None;
            return;
        }
        let path = super::identity::data_dir().join("pair_qr.jpg");
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if mtime == self.qr_mtime && self.qr_tex.is_some() {
            self.app.tablets.qr_tex = self.qr_tex.clone();
            return;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            return;
        };
        let Ok(img) = image::load_from_memory(&bytes) else {
            return;
        };
        let luma = img.to_luma8();
        let (w, h) = (luma.width() as usize, luma.height() as usize);
        let color = egui::ColorImage::from_gray([w, h], luma.as_raw());
        let tex = ctx.load_texture("pair-qr", color, egui::TextureOptions::LINEAR);
        self.qr_tex = Some(tex.clone());
        self.qr_mtime = mtime;
        self.app.tablets.qr_tex = Some(tex);
    }

    /// Start the sliders from the real configured view, not defaults.
    fn seed_view(&mut self) {
        let v = self.core.injector.lock_ok().view.clone();
        self.app.mapping.area = [v.area.0, v.area.1, v.area.2, v.area.3];
        self.app.mapping.keep_aspect = v.keep_aspect;
        self.app.mapping.rotation = v.rotation;
        self.app.pressure.curve = v.curve;
    }

    fn sync(&mut self) {
        let core = &self.core;
        // Session + pairing (one short hub lock).
        let hub = core.hub.lock_ok().status(now_secs());
        self.app.status.connected = hub.tablet.is_some();
        self.app.status.reconnecting = false;
        self.app.status.tablet_name = hub.tablet.map(|(name, _)| name).unwrap_or_default();
        self.app.status.dropped = hub.dropped;
        self.app.tablets.pairing_open = hub.pairing_open;
        self.app.tablets.countdown_s = hub.pairing_left_s;
        self.app.tablets.pin = hub.pin;
        self.app.tablets.paired = hub.paired;
        // Live pen sample.
        {
            let inj = core.injector.lock_ok();
            let l = &inj.last_pen;
            self.app.status.pen = super::screens::LivePen {
                x: l.x,
                y: l.y,
                pressure: l.pressure,
                tilt_x: l.tilt_x,
                tilt_y: l.tilt_y,
                tool: match (l.eraser, l.barrel) {
                    (true, _) => "eraser",
                    (false, true) => "pen + button",
                    _ => "pen",
                }
                .into(),
            };
            self.app.pressure.live_raw = l.pressure;
        }
        // Failsafe banner.
        {
            let fs = core.failsafe.lock_ok();
            self.app.status.paused = fs.paused().is_some();
            self.app.status.banner = match fs.paused() {
                Some(PauseReason::AutoStuck) => {
                    "Pen auto-paused after repeated stuck input. Resume when ready.".into()
                }
                Some(PauseReason::Panic) => "Emergency release active. Resume when ready.".into(),
                _ => String::new(),
            };
            self.app.settings.panic_hotkey = fs.panic_hotkey.join("+");
        }
        // Profiles.
        {
            let ps = core.profiles.lock_ok();
            self.app.status.profile = ps
                .current()
                .map(|p| p.name.clone())
                .unwrap_or_else(|| "-".into());
            self.app.status.app = ps.foreground.clone().unwrap_or_default();
            self.app.profiles.ids = ps.profiles.iter().map(|p| p.id.clone()).collect();
            self.app.profiles.selected = ps.locked.clone();
        }
        // This PC's identity, for comparing with the tablet's mismatch text.
        self.app.tablets.cert_id = super::identity::fp_hex(&core.fp).chars().take(8).collect();
        if self.app.screen == super::screens::Screen::Tablets
            && self
                .hosts_at
                .is_none_or(|t| t.elapsed() > Duration::from_secs(5))
        {
            self.app.tablets.hosts = super::server::lan_hosts();
            self.hosts_at = Some(std::time::Instant::now());
        }
        self.app.mapping.monitors = vec!["primary".to_string(), "all".to_string()];
        self.app.settings.control_port = core.cfg.control_port;
        self.app.settings.pen_port = core.cfg.pen_port;
        self.app.settings.wayland_note =
            std::env::var("XDG_SESSION_TYPE").as_deref() == Ok("wayland");
    }

    /// Mapping + pressure edits take effect immediately; nothing is pushed
    /// unless the user actually changed something.
    fn push_view(&mut self) {
        let m = &self.app.mapping;
        let want = (
            (m.area[0], m.area[1], m.area[2], m.area[3]),
            m.keep_aspect,
            m.rotation,
            self.app.pressure.curve,
        );
        if self.pushed_view.as_ref() == Some(&want) {
            return;
        }
        if self.pushed_view.is_some() {
            let mut inj = self.core.injector.lock_ok();
            inj.view.area = want.0;
            inj.view.keep_aspect = want.1;
            inj.view.rotation = want.2;
            inj.view.curve = want.3;
        }
        self.pushed_view = Some(want);
    }

    fn exec(&self, action: PendingAction) {
        match action {
            PendingAction::Panic => self.core.emergency_release(),
            PendingAction::PauseToggle => self.core.toggle_pause(),
            PendingAction::PairOpen => self.core.open_pairing(),
            PendingAction::ForgetTablet(id) => self.core.forget_tablet(&id),
            PendingAction::ProfileLock(target) => self.core.lock_profile(target.as_deref()),
        }
    }
}

/// Debug builds only: `CHIZ_SHOT_DIR=/dir` walks every screen and writes
/// `<screen>.ppm` snapshots of the real window, then exits. Used to review
/// the UI without a screenshot tool; compiled out of release builds.
#[cfg(debug_assertions)]
fn shot_hook(app: &mut GuiApp, ctx: &egui::Context, frame_no: &mut u32) {
    use super::screens::Screen;
    let Some(dir) = std::env::var_os("CHIZ_SHOT_DIR") else {
        return;
    };
    let screens = [
        ("status", Screen::Status),
        ("mapping", Screen::Mapping),
        ("pressure", Screen::Pressure),
        ("profiles", Screen::Profiles),
        ("tablets", Screen::Tablets),
        ("settings", Screen::Settings),
    ];
    for ev in ctx.input(|i| i.raw.events.clone()) {
        if let egui::Event::Screenshot {
            image, user_data, ..
        } = ev
        {
            let name = user_data
                .data
                .as_ref()
                .and_then(|d| d.downcast_ref::<String>())
                .cloned()
                .unwrap_or_default();
            let mut out = format!("P6\n{} {}\n255\n", image.width(), image.height()).into_bytes();
            for px in &image.pixels {
                out.extend_from_slice(&[px.r(), px.g(), px.b()]);
            }
            let _ = std::fs::write(std::path::Path::new(&dir).join(format!("{name}.ppm")), out);
        }
    }
    *frame_no += 1;
    let step = (*frame_no / 12) as usize; // let each screen settle ~12 frames
    if (*frame_no % 12) == 8 && step < screens.len() {
        let (name, _) = screens[step];
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(
            name.to_string(),
        )));
    }
    if (*frame_no).is_multiple_of(12) && step < screens.len() {
        app.app.screen = screens[step].1;
    }
    if step >= screens.len() && (*frame_no % 12) == 6 {
        std::process::exit(0);
    }
    ctx.request_repaint();
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        #[cfg(debug_assertions)]
        {
            let mut n = self.shot_frame;
            shot_hook(self, ctx, &mut n);
            self.shot_frame = n;
        }
        self.sync();
        self.sync_qr(ctx);
        self.app.show(ctx);
        self.push_view();
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
