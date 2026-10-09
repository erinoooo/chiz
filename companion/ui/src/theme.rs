//! Visual identity for the companion window. Same palette as the Android
//! app (near-black surfaces, amber primary, teal secondary) so the two
//! halves of Chiz read as one product. All styling lives here; screens only
//! call the helpers.

use egui::{
    epaint::Shadow, vec2, Color32, CornerRadius, FontFamily, FontId, Margin, RichText, Stroke,
    TextStyle, Ui,
};

pub const BG: Color32 = Color32::from_rgb(0x0E, 0x0E, 0x12);
pub const SIDEBAR: Color32 = Color32::from_rgb(0x12, 0x12, 0x17);
pub const SURFACE: Color32 = Color32::from_rgb(0x17, 0x17, 0x1D);
pub const SURFACE_HI: Color32 = Color32::from_rgb(0x23, 0x23, 0x2B);
pub const BORDER: Color32 = Color32::from_rgb(0x2C, 0x2C, 0x36);
pub const AMBER: Color32 = Color32::from_rgb(0xE8, 0xC5, 0x47);
pub const ON_AMBER: Color32 = Color32::from_rgb(0x1A, 0x15, 0x03);
pub const TEAL: Color32 = Color32::from_rgb(0x7D, 0xD0, 0xC3);
pub const DANGER: Color32 = Color32::from_rgb(0xF2, 0x5C, 0x54);
pub const GO: Color32 = Color32::from_rgb(0x4C, 0xC3, 0x7A);
pub const TEXT: Color32 = Color32::from_rgb(0xEC, 0xEC, 0xF1);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x9A, 0x9A, 0xA8);

fn r(v: u8) -> CornerRadius {
    CornerRadius::same(v)
}

/// Install fonts sizes, spacing and colors. Call once at startup.
pub fn apply(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.text_styles = [
        (
            TextStyle::Heading,
            FontId::new(26.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(15.0, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(15.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Small,
            FontId::new(12.5, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(14.0, FontFamily::Monospace),
        ),
    ]
    .into();
    style.spacing.item_spacing = vec2(10.0, 10.0);
    style.spacing.button_padding = vec2(16.0, 8.0);
    style.spacing.interact_size.y = 34.0;
    style.spacing.slider_width = 180.0;
    style.spacing.combo_width = 160.0;

    let v = &mut style.visuals;
    v.dark_mode = true;
    v.override_text_color = Some(TEXT);
    v.panel_fill = BG;
    v.window_fill = SURFACE;
    v.extreme_bg_color = Color32::from_rgb(0x0A, 0x0A, 0x0D);
    v.faint_bg_color = SURFACE;
    v.code_bg_color = Color32::from_rgb(0x0A, 0x0A, 0x0D);
    v.window_stroke = Stroke::new(1.0_f32, BORDER);
    v.window_corner_radius = r(14);
    v.menu_corner_radius = r(10);
    v.popup_shadow = Shadow {
        offset: [0, 6],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(140),
    };
    v.selection.bg_fill = AMBER.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0_f32, AMBER);
    v.hyperlink_color = TEAL;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = SURFACE;
    w.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    w.noninteractive.corner_radius = r(10);
    w.inactive.bg_fill = SURFACE_HI;
    w.inactive.weak_bg_fill = SURFACE_HI;
    w.inactive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    w.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    w.inactive.corner_radius = r(10);
    w.hovered.bg_fill = Color32::from_rgb(0x2E, 0x2E, 0x38);
    w.hovered.weak_bg_fill = Color32::from_rgb(0x2E, 0x2E, 0x38);
    w.hovered.bg_stroke = Stroke::new(1.0_f32, AMBER.gamma_multiply(0.6));
    w.hovered.fg_stroke = Stroke::new(1.5_f32, TEXT);
    w.hovered.corner_radius = r(10);
    w.active.bg_fill = AMBER.gamma_multiply(0.25);
    w.active.weak_bg_fill = AMBER.gamma_multiply(0.25);
    w.active.bg_stroke = Stroke::new(1.0_f32, AMBER);
    w.active.fg_stroke = Stroke::new(1.5_f32, TEXT);
    w.active.corner_radius = r(10);
    w.open = w.hovered;
    ctx.set_style(style);
}

/// A rounded surface block. Everything on screen sits in one of these.
pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .corner_radius(r(14))
        .inner_margin(Margin::same(18))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// Card whose accent border signals a state (warning / danger / ok).
pub fn alert_card(ui: &mut Ui, accent: Color32, text: &str) {
    egui::Frame::new()
        .fill(accent.gamma_multiply(0.12))
        .stroke(Stroke::new(1.0_f32, accent.gamma_multiply(0.7)))
        .corner_radius(r(12))
        .inner_margin(Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).color(accent));
        });
}

pub fn title(ui: &mut Ui, text: &str, subtitle: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(text).heading().strong());
    if !subtitle.is_empty() {
        ui.label(RichText::new(subtitle).color(TEXT_DIM));
    }
    ui.add_space(6.0);
}

pub fn section(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text.to_uppercase()).small().color(TEXT_DIM));
}

pub fn dim(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).color(TEXT_DIM)
}

/// Amber call-to-action.
pub fn primary_button(ui: &mut Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).strong().color(ON_AMBER))
            .fill(AMBER)
            .stroke(Stroke::NONE)
            .corner_radius(r(10))
            .min_size(vec2(0.0, 36.0)),
    )
}

/// Destructive / emergency action.
pub fn danger_button(ui: &mut Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).strong().color(Color32::WHITE))
            .fill(DANGER.gamma_multiply(0.85))
            .stroke(Stroke::NONE)
            .corner_radius(r(10))
            .min_size(vec2(0.0, 36.0)),
    )
}

/// Selectable chip (profile picker, presets, rotation).
pub fn chip(ui: &mut Ui, selected: bool, text: &str) -> egui::Response {
    let (fill, fg, stroke) = if selected {
        (AMBER, ON_AMBER, Stroke::NONE)
    } else {
        (SURFACE_HI, TEXT, Stroke::new(1.0_f32, BORDER))
    };
    ui.add(
        egui::Button::new(RichText::new(text).color(fg))
            .fill(fill)
            .stroke(stroke)
            .corner_radius(r(20))
            .min_size(vec2(0.0, 32.0)),
    )
}

/// Small colored status dot + label.
pub fn status_pill(ui: &mut Ui, color: Color32, text: &str) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.14))
        .corner_radius(r(20))
        .inner_margin(Margin::symmetric(12, 5))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 7.0;
                let (rect, _) = ui.allocate_exact_size(vec2(9.0, 9.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 4.5, color);
                ui.label(RichText::new(text).small().strong().color(color));
            });
        });
}

/// Label above a big value, used for the stat row.
pub fn stat(ui: &mut Ui, label: &str, value: &str, unit: &str) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.label(RichText::new(label).small().color(TEXT_DIM));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(RichText::new(value).size(26.0).strong());
            ui.label(RichText::new(unit).color(TEXT_DIM));
        });
    });
}

/// Thin horizontal meter (0..1) with an accent fill.
pub fn meter(ui: &mut Ui, fraction: f32, color: Color32, label: &str) {
    ui.horizontal(|ui| {
        ui.add_sized(vec2(64.0, 18.0), egui::Label::new(dim(label)));
        let w = (ui.available_width() - 52.0).max(40.0);
        let (rect, _) = ui.allocate_exact_size(vec2(w, 8.0), egui::Sense::hover());
        let p = ui.painter();
        p.rect_filled(rect, r(4), SURFACE_HI);
        let mut fill = rect;
        fill.set_width(rect.width() * fraction.clamp(0.0, 1.0));
        p.rect_filled(fill, r(4), color);
        ui.label(RichText::new(format!("{:.0}%", fraction * 100.0)).monospace());
    });
}
