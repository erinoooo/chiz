//! Companion screens (spec 8): Status / Mapping / Pressure / Profiles /
//! Tablets / Settings, rendered with egui. The `eframe` runner (display
//! server) lands in M7; this module is the complete screen state +
//! rendering, compile-checked here, with pure helpers unit-tested.
//!
//! Areas per spec: tray menu lives in `tray.rs`; status shows tablet name,
//! RTT, drop counters, live pen values; mapping has monitor picker, area
//! editor (numbers + drag box), keep-aspect + rotation, and a test box;
//! pressure has curve editor + presets + live meter; profiles has the list
//! + editor with full validation; tablets has Forget + QR/PIN/countdown;
//! settings has ports, login, log level, panic hotkey.

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum Screen {
    #[default]
    Status,
    Mapping,
    Pressure,
    Profiles,
    Tablets,
    Settings,
}

#[derive(Clone, Debug, Default)]
pub struct LivePen {
    pub x: f64,
    pub y: f64,
    pub pressure: f64,
    pub tilt_x: i8,
    pub tilt_y: i8,
    pub distance: f64,
    pub tool: String,
    pub barrel: String,
}

#[derive(Clone, Debug, Default)]
pub struct StatusView {
    pub tablet_name: String,
    pub connected: bool,
    pub reconnecting: bool,
    pub rtt_ms: f64,
    pub companion_us: f64,
    pub dropped: u64,
    pub pen: LivePen,
    pub paused: bool,
    pub banner: String,
}

#[derive(Clone, Debug)]
pub struct MappingView {
    pub monitors: Vec<String>,
    pub monitor_idx: usize,
    pub area: [f64; 4], // x,y,w,h in 0..1
    pub keep_aspect: bool,
    pub rotation: u16, // 0/90/180/270
    pub test_xy: (f64, f64),
}

impl Default for MappingView {
    fn default() -> Self {
        Self {
            monitors: vec![],
            monitor_idx: 0,
            area: [0.0, 0.0, 1.0, 1.0],
            keep_aspect: true,
            rotation: 0,
            test_xy: (0.5, 0.5),
        }
    }
}

impl MappingView {
    pub fn clamp_area(&mut self) {
        for v in self.area.iter_mut() {
            *v = v.clamp(0.0, 1.0);
        }
        if self.area[2] <= 0.0 {
            self.area[2] = 1.0;
        }
        if self.area[3] <= 0.0 {
            self.area[3] = 1.0;
        }
        if self.area[0] + self.area[2] > 1.0 {
            self.area[0] = 1.0 - self.area[2];
        }
        if self.area[1] + self.area[3] > 1.0 {
            self.area[1] = 1.0 - self.area[3];
        }
    }

    pub fn cycle_rotation(&mut self) {
        self.rotation = match self.rotation {
            0 => 90,
            90 => 180,
            180 => 270,
            _ => 0,
        };
    }
}

#[derive(Clone, Debug)]
pub struct PressureView {
    pub curve: [f64; 4],
    pub live_raw: f64,
}

impl Default for PressureView {
    fn default() -> Self {
        Self {
            curve: [0.25, 0.25, 0.75, 0.75],
            live_raw: 0.0,
        }
    }
}

impl PressureView {
    pub fn preset(&mut self, name: &str) {
        self.curve = match name {
            "soft" => [0.10, 0.40, 0.50, 0.90],
            "firm" => [0.50, 0.10, 0.90, 0.50],
            _ => [0.25, 0.25, 0.75, 0.75],
        };
    }

    pub fn live_mapped(&self) -> f64 {
        chiz_core::apply_pressure_curve(&self.curve, self.live_raw)
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProfilesView {
    pub ids: Vec<String>,
    pub selected: Option<String>,
    pub editor_text: String,
    pub editor_error: String,
}

#[derive(Clone, Default)]
pub struct TabletsView {
    pub paired: Vec<(String, String)>, // (device_id, name)
    pub pairing_open: bool,
    pub countdown_s: u64,
    pub pin: String,
    pub qr_tex: Option<egui::TextureHandle>,
}

#[derive(Clone, Debug)]
pub struct SettingsView {
    pub control_port: u16,
    pub pen_port: u16,
    pub start_at_login: bool,
    pub log_level: String,
    pub panic_hotkey: String,
    pub wayland_note: bool,
}

impl Default for SettingsView {
    fn default() -> Self {
        Self {
            control_port: 47800,
            pen_port: 47801,
            start_at_login: false,
            log_level: "info".into(),
            panic_hotkey: chiz_core::failsafe::DEFAULT_PANIC_HOTKEY.into(),
            wayland_note: false,
        }
    }
}

/// Button presses the GUI must execute against the runtime (the screens
/// themselves never touch locks or the network).
#[derive(Clone, Debug)]
pub enum PendingAction {
    Panic,
    PauseToggle,
    PairOpen,
    ForgetTablet(String),
    ProfileLock(Option<String>), // None = automatic
}

#[derive(Default)]
pub struct CompanionApp {
    pub pending: Vec<PendingAction>,
    pub screen: Screen,
    pub status: StatusView,
    pub mapping: MappingView,
    pub pressure: PressureView,
    pub profiles: ProfilesView,
    pub tablets: TabletsView,
    pub settings: SettingsView,
}

impl CompanionApp {
    pub fn show(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("nav").show(ctx, |ui| {
            ui.heading("Chiz");
            for (label, s) in [
                ("Status", Screen::Status),
                ("Mapping", Screen::Mapping),
                ("Pressure", Screen::Pressure),
                ("Profiles", Screen::Profiles),
                ("Tablets", Screen::Tablets),
                ("Settings", Screen::Settings),
            ] {
                if ui.selectable_label(self.screen == s, label).clicked() {
                    self.screen = s;
                }
            }
        });
        egui::CentralPanel::default().show(ctx, |ui| match self.screen {
            Screen::Status => self.status_ui(ui),
            Screen::Mapping => self.mapping_ui(ui),
            Screen::Pressure => self.pressure_ui(ui),
            Screen::Profiles => self.profiles_ui(ui),
            Screen::Tablets => self.tablets_ui(ui),
            Screen::Settings => self.settings_ui(ui),
        });
    }

    fn act(&mut self, a: PendingAction) {
        self.pending.push(a);
    }

    fn status_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Status");
        let dot = if self.status.connected {
            egui::Color32::GREEN
        } else if self.status.reconnecting {
            egui::Color32::YELLOW
        } else {
            egui::Color32::RED
        };
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("●").color(dot));
            ui.label(if self.status.tablet_name.is_empty() {
                "no tablet".to_string()
            } else {
                self.status.tablet_name.clone()
            });
        });
        egui::Grid::new("status").show(ui, |ui| {
            ui.label("Round-trip");
            ui.label(format!("{:.1} ms", self.status.rtt_ms));
            ui.end_row();
            ui.label("Companion processing");
            ui.label(format!("{:.2} ms", self.status.companion_us / 1000.0));
            ui.end_row();
            ui.label("Dropped datagrams");
            ui.label(self.status.dropped.to_string());
            ui.end_row();
            let p = &self.status.pen;
            ui.label("Pen");
            ui.label(format!(
                "x={:.3} y={:.3} p={:.3} tilt={}/{} d={:.2} {} {}",
                p.x, p.y, p.pressure, p.tilt_x, p.tilt_y, p.distance, p.tool, p.barrel
            ));
            ui.end_row();
        });
        if self.status.paused {
            ui.label(egui::RichText::new("Pen input paused. Buttons keep working.").strong());
        }
        ui.horizontal(|ui| {
            if ui.button("Emergency release").clicked() {
                self.act(PendingAction::Panic);
            }
            let pause_label = if self.status.paused { "Resume pen input" } else { "Pause pen input" };
            if ui.button(pause_label).clicked() {
                self.act(PendingAction::PauseToggle);
            }
        });
        if !self.status.banner.is_empty() {
            ui.label(egui::RichText::new(&self.status.banner).color(egui::Color32::YELLOW));
        }
    }

    fn mapping_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Mapping");
        if self.mapping.monitors.is_empty() {
            ui.label("No monitors enumerated yet.");
        } else {
            egui::ComboBox::from_label("Monitor")
                .selected_text(self.mapping.monitors[self.mapping.monitor_idx].clone())
                .show_ui(ui, |ui| {
                    for (i, m) in self.mapping.monitors.iter().enumerate() {
                        ui.selectable_value(&mut self.mapping.monitor_idx, i, m);
                    }
                });
        }
        ui.horizontal(|ui| {
            for (i, name) in ["x", "y", "w", "h"].iter().enumerate() {
                ui.label(*name);
                ui.add(egui::DragValue::new(&mut self.mapping.area[i]).speed(0.01));
            }
        });
        self.mapping.clamp_area();
        ui.checkbox(&mut self.mapping.keep_aspect, "Lock aspect (circles stay circles)");
        if ui.button(format!("Rotation: {}°", self.mapping.rotation)).clicked() {
            self.mapping.cycle_rotation();
        }
        // Test box: where the cursor lands for the probe point.
        ui.label("Test: drag the probe, see the mapped pixel.");
        ui.horizontal(|ui| {
            ui.add(egui::Slider::new(&mut self.mapping.test_xy.0, 0.0..=1.0).text("u"));
            ui.add(egui::Slider::new(&mut self.mapping.test_xy.1, 0.0..=1.0).text("v"));
        });
        let (px, py) = chiz_core::map_pen_to_px(
            self.mapping.test_xy.0,
            self.mapping.test_xy.1,
            2100.0,
            1600.0,
            0.0,
            0.0,
            1920.0,
            1080.0,
            (
                self.mapping.area[0],
                self.mapping.area[1],
                self.mapping.area[2],
                self.mapping.area[3],
            ),
            self.mapping.keep_aspect,
            self.mapping.rotation,
        );
        ui.label(format!("lands at {px}, {py}"));
    }

    fn pressure_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Pressure");
        ui.horizontal(|ui| {
            for name in ["linear", "soft", "firm"] {
                if ui.button(name).clicked() {
                    self.pressure.preset(name);
                }
            }
        });
        ui.horizontal(|ui| {
            for (i, name) in ["x1", "y1", "x2", "y2"].iter().enumerate() {
                ui.label(*name);
                ui.add(egui::Slider::new(&mut self.pressure.curve[i], 0.0..=1.0));
            }
        });
        ui.add(egui::ProgressBar::new(self.pressure.live_raw as f32).text("raw"));
        ui.add(egui::ProgressBar::new(self.pressure.live_mapped() as f32).text("mapped"));
    }

    fn profiles_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Profiles");
        if ui.button("Auto (follow active app)").clicked() {
            self.act(PendingAction::ProfileLock(None));
        }
        ui.horizontal(|ui| {
            for id in self.profiles.ids.clone() {
                if ui
                    .selectable_label(self.profiles.selected.as_deref() == Some(&id), &id)
                    .clicked()
                {
                    self.profiles.selected = Some(id.clone());
                    self.act(PendingAction::ProfileLock(Some(id)));
                }
            }
        });
        ui.label("Editor (JSON, validated on save):");
        ui.text_edit_multiline(&mut self.profiles.editor_text);
        if !self.profiles.editor_error.is_empty() {
            ui.label(egui::RichText::new(&self.profiles.editor_error).color(egui::Color32::RED));
        }
    }

    /// Validate the editor buffer with the core rules (spec 11).
    pub fn validate_editor(&mut self) {
        match serde_json::from_str::<chiz_core::profile::Profile>(&self.profiles.editor_text) {
            Ok(p) => {
                let errs = chiz_core::profile::validate_profile(&p);
                self.profiles.editor_error = errs.join("; ");
            }
            Err(e) => self.profiles.editor_error = format!("JSON: {e}"),
        }
    }

    fn tablets_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Tablets");
        ui.label("Prefer QR pairing. Use PIN pairing only on a network you trust.");
        let paired = self.tablets.paired.clone();
        for (id, name) in &paired {
            ui.horizontal(|ui| {
                ui.label(format!("{name} ({id})"));
                if ui.button("Forget").clicked() {
                    self.act(PendingAction::ForgetTablet(id.clone()));
                }
            });
        }
        if self.tablets.pairing_open {
            ui.label(format!(
                "Pairing open: {} s left — PIN {}",
                self.tablets.countdown_s, self.tablets.pin
            ));
            ui.label("Scan the QR below, or type the PIN on the tablet (trusted networks only).");
            if let Some(tex) = &self.tablets.qr_tex {
                ui.image(tex);
            }
        } else if ui.button("Pair new tablet").clicked() {
            self.act(PendingAction::PairOpen);
        }
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Settings");
        ui.horizontal(|ui| {
            ui.label("Control port");
            ui.add(egui::DragValue::new(&mut self.settings.control_port));
            ui.label("Pen port");
            ui.add(egui::DragValue::new(&mut self.settings.pen_port));
        });
        ui.checkbox(&mut self.settings.start_at_login, "Start at login");
        ui.horizontal(|ui| {
            ui.label("Log level");
            egui::ComboBox::from_id_salt("log")
                .selected_text(&self.settings.log_level)
                .show_ui(ui, |ui| {
                    for l in ["error", "warn", "info", "debug"] {
                        ui.selectable_value(&mut self.settings.log_level, l.to_string(), l);
                    }
                });
        });
        ui.horizontal(|ui| {
            ui.label("Panic hotkey");
            ui.text_edit_singleline(&mut self.settings.panic_hotkey);
        });
        if self.settings.wayland_note {
            ui.label("Automatic profile switching is unavailable on Wayland. Choose a profile from the tray.");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_helpers() {
        let mut m = MappingView::default();
        m.area = [0.9, 0.9, 0.5, 0.5];
        m.clamp_area();
        assert!(m.area[0] + m.area[2] <= 1.0 + 1e-9);
        m.rotation = 0;
        m.cycle_rotation();
        assert_eq!(m.rotation, 90);
        m.rotation = 270;
        m.cycle_rotation();
        assert_eq!(m.rotation, 0);
    }

    #[test]
    fn pressure_presets_match_spec() {
        let mut p = PressureView::default();
        p.preset("soft");
        assert!((p.curve[1] - 0.40).abs() < 1e-9);
        p.live_raw = 0.5;
        assert!((p.live_mapped() - 0.7532).abs() <= 0.002);
        p.preset("linear");
        assert!((p.live_mapped() - 0.5).abs() <= 0.002);
    }

    #[test]
    fn editor_validation_reports() {
        let mut app = CompanionApp::default();
        app.profiles.editor_text = "not json".into();
        app.validate_editor();
        assert!(app.profiles.editor_error.contains("JSON"));
        app.profiles.editor_text = serde_json::to_string_pretty(
            &serde_json::json!({
                "version": 1, "id": "x", "name": "X",
                "grid": {"cols": 1, "rows": 1}, "buttons": [],
            }),
        )
        .unwrap();
        app.validate_editor();
        assert!(app.profiles.editor_error.is_empty());
    }
}
