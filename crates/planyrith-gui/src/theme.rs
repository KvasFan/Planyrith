//! The look of the shell (docs/07 §1, §12): a light and a dark theme built
//! on egui's, with one accent color, quieter widget backgrounds, rounded
//! corners and roomier spacing. The user picks System, Light or Dark (View ›
//! Theme); the choice is a per-user setting, never document state.

use egui::{Color32, CornerRadius, Margin, Stroke, Theme, ThemePreference, Visuals};
use serde::{Deserialize, Serialize};

/// The accent: selection, active tool, focused widgets.
pub const ACCENT: Color32 = Color32::from_rgb(0x2F, 0x6F, 0xDE);

/// View › Theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ThemeChoice {
    /// Follow the operating system.
    #[default]
    System,
    /// Always light.
    Light,
    /// Always dark.
    Dark,
}

impl ThemeChoice {
    /// Menu label.
    pub fn label(self) -> &'static str {
        match self {
            ThemeChoice::System => "System",
            ThemeChoice::Light => "Light",
            ThemeChoice::Dark => "Dark",
        }
    }
}

fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

fn light() -> Visuals {
    let mut v = Visuals::light();
    let text = rgb(0x1F2328);
    v.panel_fill = rgb(0xF3F4F6);
    v.window_fill = rgb(0xFBFBFC);
    v.faint_bg_color = rgb(0xECEEF1);
    v.extreme_bg_color = rgb(0xFFFFFF);
    v.window_stroke = Stroke::new(1.0, rgb(0xD0D4DA));
    v.selection.bg_fill = rgb(0xCFE0FB);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;
    let w = &mut v.widgets;
    w.noninteractive.bg_stroke = Stroke::new(1.0, rgb(0xDADDE2));
    w.noninteractive.fg_stroke = Stroke::new(1.0, text);
    w.inactive.weak_bg_fill = rgb(0xE7E9ED);
    w.inactive.bg_fill = rgb(0xE7E9ED);
    w.inactive.fg_stroke = Stroke::new(1.0, text);
    w.inactive.bg_stroke = Stroke::new(1.0, rgb(0xC6CBD3));
    w.hovered.weak_bg_fill = rgb(0xDCE3EC);
    w.hovered.bg_fill = rgb(0xDCE3EC);
    w.hovered.bg_stroke = Stroke::new(1.0, rgb(0xB7C3D3));
    w.hovered.fg_stroke = Stroke::new(1.5, text);
    w.active.weak_bg_fill = rgb(0xC9D7EA);
    w.active.bg_fill = rgb(0xC9D7EA);
    w.active.bg_stroke = Stroke::new(1.0, ACCENT);
    w.open.weak_bg_fill = rgb(0xE1E6ED);
    v
}

fn dark() -> Visuals {
    let mut v = Visuals::dark();
    let text = rgb(0xE6E8EB);
    v.panel_fill = rgb(0x24272C);
    v.window_fill = rgb(0x2A2E34);
    v.faint_bg_color = rgb(0x2C3036);
    v.extreme_bg_color = rgb(0x17191C);
    v.window_stroke = Stroke::new(1.0, rgb(0x3A3F47));
    v.selection.bg_fill = rgb(0x24467F);
    v.selection.stroke = Stroke::new(1.0, rgb(0x8DB4F5));
    v.hyperlink_color = rgb(0x8DB4F5);
    let w = &mut v.widgets;
    w.noninteractive.bg_stroke = Stroke::new(1.0, rgb(0x383D45));
    w.noninteractive.fg_stroke = Stroke::new(1.0, text);
    w.inactive.weak_bg_fill = rgb(0x31353C);
    w.inactive.bg_fill = rgb(0x31353C);
    w.inactive.fg_stroke = Stroke::new(1.0, text);
    w.inactive.bg_stroke = Stroke::new(1.0, rgb(0x4A5059));
    w.hovered.weak_bg_fill = rgb(0x3A4049);
    w.hovered.bg_fill = rgb(0x3A4049);
    w.hovered.bg_stroke = Stroke::new(1.0, rgb(0x56606D));
    w.hovered.fg_stroke = Stroke::new(1.5, text);
    w.active.weak_bg_fill = rgb(0x2D4B7C);
    w.active.bg_fill = rgb(0x2D4B7C);
    w.active.bg_stroke = Stroke::new(1.0, rgb(0x8DB4F5));
    w.open.weak_bg_fill = rgb(0x353A42);
    v
}

/// Installs both themes and selects `choice`.
pub fn apply(ctx: &egui::Context, choice: ThemeChoice) {
    ctx.set_visuals_of(Theme::Light, light());
    ctx.set_visuals_of(Theme::Dark, dark());
    ctx.all_styles_mut(|style| {
        let radius = CornerRadius::same(4);
        let v = &mut style.visuals;
        v.window_corner_radius = CornerRadius::same(6);
        v.menu_corner_radius = CornerRadius::same(5);
        for w in [
            &mut v.widgets.noninteractive,
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.corner_radius = radius;
        }
        let s = &mut style.spacing;
        s.item_spacing = egui::vec2(8.0, 5.0);
        s.button_padding = egui::vec2(8.0, 3.0);
        s.menu_margin = Margin::same(6);
        s.interact_size.y = 22.0;
    });
    ctx.set_theme(match choice {
        ThemeChoice::System => ThemePreference::System,
        ThemeChoice::Light => ThemePreference::Light,
        ThemeChoice::Dark => ThemePreference::Dark,
    });
}
