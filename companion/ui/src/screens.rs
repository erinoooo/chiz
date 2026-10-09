//! Companion screens (spec 8): Status / Mapping / Pressure / Profiles /
//! Tablets / Settings, rendered with egui by `gui.rs`; pure helpers are
//! unit-tested.
//!
//! Pages: status (connection, profile, live pen), mapping (area preview),
//! pressure (curve plot), profiles, tablets (pairing QR/PIN), settings.

use super::theme;

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
    pub tool: String,
}

#[derive(Clone, Debug, Default)]
pub struct StatusView {
    pub tablet_name: String,
    pub connected: bool,
    pub reconnecting: bool,
    pub profile: String,
    pub app: String,
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

    #[cfg(test)]
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
    pub panic_hotkey: String,
    pub wayland_note: bool,
}

impl Default for SettingsView {
    fn default() -> Self {
        Self {
            control_port: 47800,
            pen_port: 47801,
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
        egui::SidePanel::left("nav")
            .exact_width(210.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(theme::SIDEBAR)
                    .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
                    .inner_margin(egui::Margin::symmetric(14, 18)),
            )
            .show(ctx, |ui| self.nav_ui(ui));
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(theme::BG)
                    .inner_margin(egui::Margin {
                        left: 28,
                        right: 28,
                        top: 18,
                        bottom: 18,
                    }),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_max_width(ui.available_width().min(860.0));
                        match self.screen {
                            Screen::Status => self.status_ui(ui),
                            Screen::Mapping => self.mapping_ui(ui),
                            Screen::Pressure => self.pressure_ui(ui),
                            Screen::Profiles => self.profiles_ui(ui),
                            Screen::Tablets => self.tablets_ui(ui),
                            Screen::Settings => self.settings_ui(ui),
                        }
                    });
            });
    }

    fn act(&mut self, a: PendingAction) {
        self.pending.push(a);
    }

    fn connection_color(&self) -> egui::Color32 {
        if self.status.paused {
            theme::AMBER
        } else if self.status.connected {
            theme::GO
        } else if self.status.reconnecting {
            theme::AMBER
        } else {
            theme::TEXT_DIM
        }
    }

    fn nav_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.label(
                egui::RichText::new("Chiz")
                    .size(28.0)
                    .strong()
                    .color(theme::AMBER),
            );
            ui.label(egui::RichText::new("çiz").color(theme::TEXT_DIM));
        });
        ui.label(theme::dim("pen tablet for your PC").size(12.5));
        ui.add_space(18.0);
        for (label, s) in [
            ("Status", Screen::Status),
            ("Mapping", Screen::Mapping),
            ("Pressure", Screen::Pressure),
            ("Profiles", Screen::Profiles),
            ("Tablets", Screen::Tablets),
            ("Settings", Screen::Settings),
        ] {
            let active = self.screen == s;
            let (rect, resp) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 38.0), egui::Sense::click());
            let hover = resp.hovered();
            if active || hover {
                ui.painter().rect_filled(
                    rect,
                    egui::CornerRadius::same(10),
                    if active {
                        theme::AMBER.gamma_multiply(0.14)
                    } else {
                        theme::SURFACE_HI
                    },
                );
            }
            if active {
                let bar = egui::Rect::from_min_size(
                    rect.left_center() - egui::vec2(0.0, 11.0),
                    egui::vec2(3.0, 22.0),
                );
                ui.painter()
                    .rect_filled(bar, egui::CornerRadius::same(2), theme::AMBER);
            }
            ui.painter().text(
                rect.left_center() + egui::vec2(16.0, 0.0),
                egui::Align2::LEFT_CENTER,
                label,
                egui::FontId::proportional(15.5),
                if active { theme::AMBER } else { theme::TEXT },
            );
            if resp.clicked() {
                self.screen = s;
            }
        }
        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            let (c, t) = self.connection_summary();
            theme::status_pill(ui, c, &t);
        });
    }

    fn connection_summary(&self) -> (egui::Color32, String) {
        let c = self.connection_color();
        let t = if self.status.paused {
            "Pen paused".to_string()
        } else if self.status.connected {
            "Tablet connected".to_string()
        } else {
            "Waiting for tablet".to_string()
        };
        (c, t)
    }

    fn status_ui(&mut self, ui: &mut egui::Ui) {
        theme::title(ui, "Status", "Live view of your tablet connection.");
        if !self.status.banner.is_empty() {
            theme::alert_card(ui, theme::AMBER, &self.status.banner.clone());
        }
        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                let (c, summary) = self.connection_summary();
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(46.0, 46.0), egui::Sense::hover());
                let p = ui.painter();
                p.circle_filled(rect.center(), 23.0, c.gamma_multiply(0.16));
                p.circle_filled(rect.center(), 8.0, c);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    let headline = if self.status.connected {
                        self.status.tablet_name.clone()
                    } else {
                        "No tablet connected".to_string()
                    };
                    ui.label(egui::RichText::new(headline).size(20.0).strong());
                    ui.label(theme::dim(if self.status.connected {
                        summary
                    } else {
                        "Open the Chiz app on your tablet. Pair it first from the Tablets page."
                            .to_string()
                    }));
                });
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if theme::danger_button(ui, "Emergency release").clicked() {
                    self.act(PendingAction::Panic);
                }
                let label = if self.status.paused {
                    "Resume pen"
                } else {
                    "Pause pen"
                };
                let clicked = if self.status.paused {
                    theme::primary_button(ui, label).clicked()
                } else {
                    ui.button(label).clicked()
                };
                if clicked {
                    self.act(PendingAction::PauseToggle);
                }
            });
        });
        ui.columns(3, |cols| {
            for (col, (label, value, unit)) in cols.iter_mut().zip([
                ("PROFILE", self.status.profile.clone(), ""),
                (
                    "ACTIVE APP",
                    if self.status.app.is_empty() {
                        "-".to_string()
                    } else {
                        self.status.app.clone()
                    },
                    "",
                ),
                ("DROPPED", self.status.dropped.to_string(), "packets"),
            ]) {
                theme::card(col, |ui| theme::stat(ui, label, &value, unit));
            }
        });
        theme::card(ui, |ui| {
            theme::section(ui, "Live pen");
            let p = &self.status.pen;
            theme::meter(ui, p.pressure as f32, theme::AMBER, "Pressure");
            ui.horizontal_wrapped(|ui| {
                for (k, v) in [
                    ("Tilt", format!("{}° / {}°", p.tilt_x, p.tilt_y)),
                    (
                        "Tool",
                        if p.tool.is_empty() {
                            "-".into()
                        } else {
                            p.tool.clone()
                        },
                    ),
                    ("Position", format!("{:.3}, {:.3}", p.x, p.y)),
                ] {
                    ui.label(theme::dim(k));
                    ui.label(egui::RichText::new(v).monospace());
                    ui.add_space(14.0);
                }
            });
        });
    }

    fn mapping_ui(&mut self, ui: &mut egui::Ui) {
        theme::title(
            ui,
            "Mapping",
            "Choose which part of the screen the tablet covers.",
        );
        theme::card(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(300.0);
                    if !self.mapping.monitors.is_empty() {
                        theme::section(ui, "Monitor");
                        let idx = self
                            .mapping
                            .monitor_idx
                            .min(self.mapping.monitors.len() - 1);
                        egui::ComboBox::from_id_salt("monitor")
                            .selected_text(self.mapping.monitors[idx].clone())
                            .show_ui(ui, |ui| {
                                for (i, m) in self.mapping.monitors.iter().enumerate() {
                                    ui.selectable_value(&mut self.mapping.monitor_idx, i, m);
                                }
                            });
                    }
                    theme::section(ui, "Active area");
                    egui::Grid::new("area").num_columns(2).show(ui, |ui| {
                        for (i, name) in ["Left", "Top", "Width", "Height"].iter().enumerate() {
                            ui.label(theme::dim(*name));
                            ui.add(
                                egui::DragValue::new(&mut self.mapping.area[i])
                                    .speed(0.005)
                                    .range(0.0..=1.0)
                                    .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
                                    .custom_parser(|s| {
                                        s.trim_end_matches('%')
                                            .parse::<f64>()
                                            .ok()
                                            .map(|v| v / 100.0)
                                    }),
                            );
                            ui.end_row();
                        }
                    });
                    self.mapping.clamp_area();
                    ui.checkbox(
                        &mut self.mapping.keep_aspect,
                        "Keep aspect (circles stay circles)",
                    );
                    theme::section(ui, "Rotation");
                    ui.horizontal(|ui| {
                        for deg in [0u16, 90, 180, 270] {
                            if theme::chip(ui, self.mapping.rotation == deg, &format!("{deg}°"))
                                .clicked()
                            {
                                self.mapping.rotation = deg;
                            }
                        }
                    });
                    if ui.button("Reset to full screen").clicked() {
                        self.mapping.area = [0.0, 0.0, 1.0, 1.0];
                    }
                });
                ui.add_space(12.0);
                ui.vertical(|ui| self.mapping_preview(ui));
            });
        });
    }

    /// Miniature monitor with the active area and a draggable test probe.
    fn mapping_preview(&mut self, ui: &mut egui::Ui) {
        theme::section(ui, "Preview");
        let w = ui.available_width().clamp(240.0, 420.0);
        let (rect, resp) =
            ui.allocate_exact_size(egui::vec2(w, w * 9.0 / 16.0), egui::Sense::click_and_drag());
        let p = ui.painter();
        p.rect_filled(
            rect,
            egui::CornerRadius::same(8),
            egui::Color32::from_rgb(8, 8, 11),
        );
        p.rect_stroke(
            rect,
            egui::CornerRadius::same(8),
            egui::Stroke::new(2.0_f32, theme::BORDER),
            egui::StrokeKind::Inside,
        );
        let a = self.mapping.area;
        let area = egui::Rect::from_min_size(
            rect.min + egui::vec2(a[0] as f32 * rect.width(), a[1] as f32 * rect.height()),
            egui::vec2(a[2] as f32 * rect.width(), a[3] as f32 * rect.height()),
        );
        p.rect_filled(
            area,
            egui::CornerRadius::same(4),
            theme::AMBER.gamma_multiply(0.18),
        );
        p.rect_stroke(
            area,
            egui::CornerRadius::same(4),
            egui::Stroke::new(1.5_f32, theme::AMBER),
            egui::StrokeKind::Inside,
        );
        if let Some(pos) = resp.interact_pointer_pos() {
            let u = ((pos.x - area.min.x) / area.width().max(1.0)).clamp(0.0, 1.0);
            let v = ((pos.y - area.min.y) / area.height().max(1.0)).clamp(0.0, 1.0);
            self.mapping.test_xy = (u as f64, v as f64);
        }
        let probe = egui::pos2(
            area.min.x + self.mapping.test_xy.0 as f32 * area.width(),
            area.min.y + self.mapping.test_xy.1 as f32 * area.height(),
        );
        ui.painter().circle_filled(probe, 6.0, theme::TEAL);
        ui.painter()
            .circle_stroke(probe, 10.0, egui::Stroke::new(1.0_f32, theme::TEAL));
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
        ui.label(theme::dim(format!(
            "Click inside the area to test. Probe lands at {px}, {py}"
        )));
    }

    fn pressure_ui(&mut self, ui: &mut egui::Ui) {
        theme::title(
            ui,
            "Pressure",
            "Shape how hard you press into how thick the line gets.",
        );
        theme::card(ui, |ui| {
            ui.horizontal_top(|ui| {
                self.curve_plot(ui);
                ui.add_space(14.0);
                ui.vertical(|ui| {
                    theme::section(ui, "Presets");
                    ui.horizontal(|ui| {
                        for (name, label) in
                            [("linear", "Linear"), ("soft", "Soft"), ("firm", "Firm")]
                        {
                            let mut probe = PressureView::default();
                            probe.preset(name);
                            let on = probe
                                .curve
                                .iter()
                                .zip(self.pressure.curve.iter())
                                .all(|(a, b)| (a - b).abs() < 1e-6);
                            if theme::chip(ui, on, label).clicked() {
                                self.pressure.preset(name);
                            }
                        }
                    });
                    theme::section(ui, "Curve handles");
                    for (i, name) in ["Handle 1 x", "Handle 1 y", "Handle 2 x", "Handle 2 y"]
                        .iter()
                        .enumerate()
                    {
                        ui.horizontal(|ui| {
                            ui.add_sized(
                                egui::vec2(80.0, 20.0),
                                egui::Label::new(theme::dim(*name)),
                            );
                            ui.add(egui::Slider::new(&mut self.pressure.curve[i], 0.0..=1.0));
                        });
                    }
                });
            });
        });
        theme::card(ui, |ui| {
            theme::section(ui, "Live test");
            theme::meter(ui, self.pressure.live_raw as f32, theme::TEXT_DIM, "Raw");
            theme::meter(
                ui,
                self.pressure.live_mapped() as f32,
                theme::AMBER,
                "Output",
            );
            ui.label(theme::dim("Press on the tablet to see the curve respond."));
        });
    }

    /// Cubic-bezier pressure curve with the live operating point.
    fn curve_plot(&self, ui: &mut egui::Ui) {
        let side = 230.0;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
        let p = ui.painter();
        p.rect_filled(
            rect,
            egui::CornerRadius::same(10),
            egui::Color32::from_rgb(10, 10, 13),
        );
        for i in 1..4 {
            let t = i as f32 / 4.0;
            let g = egui::Stroke::new(1.0_f32, theme::BORDER.gamma_multiply(0.6));
            p.line_segment(
                [
                    egui::pos2(rect.min.x + t * side, rect.min.y),
                    egui::pos2(rect.min.x + t * side, rect.max.y),
                ],
                g,
            );
            p.line_segment(
                [
                    egui::pos2(rect.min.x, rect.min.y + t * side),
                    egui::pos2(rect.max.x, rect.min.y + t * side),
                ],
                g,
            );
        }
        let to =
            |x: f64, y: f64| egui::pos2(rect.min.x + x as f32 * side, rect.max.y - y as f32 * side);
        let c = self.pressure.curve;
        p.line_segment(
            [to(0.0, 0.0), to(1.0, 1.0)],
            egui::Stroke::new(1.0_f32, theme::TEXT_DIM.gamma_multiply(0.5)),
        );
        let pts: Vec<egui::Pos2> = (0..=64)
            .map(|i| {
                let t = i as f64 / 64.0;
                let u = 1.0 - t;
                let b = |a: f64, b1: f64, b2: f64, d: f64| {
                    u * u * u * a + 3.0 * u * u * t * b1 + 3.0 * u * t * t * b2 + t * t * t * d
                };
                to(b(0.0, c[0], c[2], 1.0), b(0.0, c[1], c[3], 1.0))
            })
            .collect();
        p.add(egui::Shape::line(
            pts,
            egui::Stroke::new(2.5_f32, theme::AMBER),
        ));
        p.circle_filled(to(c[0], c[1]), 4.0, theme::TEAL);
        p.circle_filled(to(c[2], c[3]), 4.0, theme::TEAL);
        let (lx, ly) = (self.pressure.live_raw, self.pressure.live_mapped());
        if lx > 0.0 {
            p.circle_filled(to(lx, ly), 5.0, egui::Color32::WHITE);
        }
    }

    fn profiles_ui(&mut self, ui: &mut egui::Ui) {
        theme::title(
            ui,
            "Profiles",
            "Button layouts. Auto follows the app you are drawing in.",
        );
        theme::card(ui, |ui| {
            theme::section(ui, "Active profile");
            ui.horizontal_wrapped(|ui| {
                if theme::chip(ui, self.profiles.selected.is_none(), "Auto").clicked() {
                    self.profiles.selected = None;
                    self.act(PendingAction::ProfileLock(None));
                }
                for id in self.profiles.ids.clone() {
                    let on = self.profiles.selected.as_deref() == Some(&id);
                    if theme::chip(ui, on, &id).clicked() {
                        self.profiles.selected = Some(id.clone());
                        self.act(PendingAction::ProfileLock(Some(id)));
                    }
                }
            });
            if self.settings.wayland_note {
                ui.label(theme::dim(
                    "Automatic switching is unavailable on Wayland. Pick a profile here.",
                ));
            }
        });
        theme::card(ui, |ui| {
            theme::section(ui, "Custom layouts");
            ui.label(theme::dim(
                "Profiles are JSON files. Put your own in a `profiles` folder next to the Chiz \
                 executable (copy default.json to start). Restart Chiz to load changes.",
            ));
        });
    }

    fn tablets_ui(&mut self, ui: &mut egui::Ui) {
        theme::title(ui, "Tablets", "Pair once, then it reconnects by itself.");
        if self.tablets.pairing_open {
            theme::card(ui, |ui| {
                ui.horizontal_top(|ui| {
                    if let Some(tex) = &self.tablets.qr_tex {
                        egui::Frame::new()
                            .fill(egui::Color32::WHITE)
                            .corner_radius(egui::CornerRadius::same(12))
                            .inner_margin(egui::Margin::same(12))
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Image::new(tex)
                                        .fit_to_exact_size(egui::vec2(220.0, 220.0)),
                                );
                            });
                    }
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new("Scan with the Chiz app")
                                .size(20.0)
                                .strong(),
                        );
                        ui.label(theme::dim(
                            "Or type this PIN on the tablet (trusted networks only):",
                        ));
                        let pin = if self.tablets.pin.len() == 8 {
                            format!("{} {}", &self.tablets.pin[..4], &self.tablets.pin[4..])
                        } else {
                            self.tablets.pin.clone()
                        };
                        ui.label(
                            egui::RichText::new(pin)
                                .monospace()
                                .size(34.0)
                                .strong()
                                .color(theme::AMBER),
                        );
                        ui.add(
                            egui::ProgressBar::new(self.tablets.countdown_s as f32 / 120.0)
                                .fill(theme::AMBER)
                                .text(format!("{} s left", self.tablets.countdown_s)),
                        );
                    });
                });
            });
        } else {
            theme::card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new("Add a tablet").size(18.0).strong());
                        ui.label(theme::dim(
                            "Opens a 2-minute window with a QR code. Prefer QR over PIN.",
                        ));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::primary_button(ui, "Pair new tablet").clicked() {
                            self.act(PendingAction::PairOpen);
                        }
                    });
                });
            });
        }
        theme::card(ui, |ui| {
            theme::section(ui, "Paired tablets");
            let paired = self.tablets.paired.clone();
            if paired.is_empty() {
                ui.label(theme::dim("None yet."));
            }
            for (id, name) in &paired {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(egui::RichText::new(name).strong());
                        let short: String = id.chars().take(8).collect();
                        ui.label(theme::dim(format!("{short}…")).small());
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button(egui::RichText::new("Forget").color(theme::DANGER))
                            .clicked()
                        {
                            self.act(PendingAction::ForgetTablet(id.clone()));
                        }
                    });
                });
            }
        });
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        theme::title(ui, "Settings", "Read from config.json at startup.");
        theme::card(ui, |ui| {
            theme::section(ui, "Network");
            egui::Grid::new("net")
                .num_columns(2)
                .spacing([24.0, 10.0])
                .show(ui, |ui| {
                    ui.label(theme::dim("Control port (TCP, TLS 1.3)"));
                    ui.label(
                        egui::RichText::new(self.settings.control_port.to_string()).monospace(),
                    );
                    ui.end_row();
                    ui.label(theme::dim("Pen port (UDP, encrypted)"));
                    ui.label(egui::RichText::new(self.settings.pen_port.to_string()).monospace());
                    ui.end_row();
                });
        });
        theme::card(ui, |ui| {
            theme::section(ui, "Safety");
            ui.horizontal(|ui| {
                ui.label(theme::dim("Panic hotkey"));
                ui.label(
                    egui::RichText::new(&self.settings.panic_hotkey)
                        .monospace()
                        .color(theme::AMBER),
                );
            });
            ui.label(theme::dim(
                "Works from any app. Also: 3-finger tap on the tablet strip.",
            ));
            if self.settings.wayland_note {
                ui.label(theme::dim(
                    "Wayland session: the global hotkey needs X11. Use the Emergency release button.",
                ));
            }
        });
        ui.label(theme::dim(
            "Edit config.json (see README) and restart Chiz to change these.",
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_helpers() {
        let mut m = MappingView {
            area: [0.9, 0.9, 0.5, 0.5],
            ..Default::default()
        };
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
}
