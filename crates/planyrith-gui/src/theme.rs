//! The look of the shell (docs/07 §1, §12; Decisions, 2026-11-03): a dark
//! and a light theme of neutral grey surfaces whose brightness, not their
//! borders, separates them — a header band for the menus, toolbar and status
//! bar, the window's panels, and darker views and fields. Controls are flat,
//! with a soft fill that brightens on hover and when pressed; corners are
//! round; one blue accent marks selection and focus. The user picks System,
//! Light or Dark (View › Theme); the choice is a per-user setting, never
//! document state.

use egui::epaint::Shadow;
use egui::{Color32, CornerRadius, Margin, Stroke, Theme, ThemePreference, Visuals};
use serde::{Deserialize, Serialize};

/// The accent of the light theme: selection, active tool, focus.
pub const ACCENT: Color32 = Color32::from_rgb(0x1C, 0x71, 0xD8);
/// The accent of the dark theme, a little brighter to stand out on grey.
pub const ACCENT_DARK: Color32 = Color32::from_rgb(0x35, 0x84, 0xE4);

/// View › Theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ThemeChoice {
    /// Follow the operating system.
    System,
    /// Always light.
    Light,
    /// Always dark (the default).
    #[default]
    Dark,
}

impl ThemeChoice {
    /// Menu label.
    pub fn label(self) -> &'static str {
        match self {
            ThemeChoice::System => crate::i18n::en("System"),
            ThemeChoice::Light => crate::i18n::en("Light"),
            ThemeChoice::Dark => crate::i18n::en("Dark"),
        }
    }
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// The surfaces and inks of one theme.
struct Palette {
    header: Color32,
    window: Color32,
    view: Color32,
    card: Color32,
    control: Color32,
    hover: Color32,
    pressed: Color32,
    border: Color32,
    text: Color32,
    weak_text: Color32,
    accent: Color32,
    accent_text: Color32,
    selection: Color32,
    shadow: Color32,
}

const DARK: Palette = Palette {
    header: rgb(0x2E2E32),
    window: rgb(0x242428),
    view: rgb(0x1C1C1F),
    card: rgb(0x2C2C31),
    control: rgb(0x38383D),
    hover: rgb(0x434349),
    pressed: rgb(0x505057),
    border: rgb(0x38383C),
    text: rgb(0xEDEDF0),
    weak_text: rgb(0x9C9CA4),
    accent: ACCENT_DARK,
    accent_text: rgb(0x78AEED),
    selection: rgb(0x2A5C99),
    shadow: Color32::from_black_alpha(110),
};

const LIGHT: Palette = Palette {
    header: rgb(0xEBEBED),
    window: rgb(0xFAFAFB),
    view: rgb(0xFFFFFF),
    card: rgb(0xF2F2F4),
    control: rgb(0xE3E3E7),
    hover: rgb(0xD8D8DD),
    pressed: rgb(0xCBCBD1),
    border: rgb(0xDADADE),
    text: rgb(0x2B2B30),
    weak_text: rgb(0x6E6E76),
    accent: ACCENT,
    accent_text: ACCENT,
    selection: rgb(0xC5DBF7),
    shadow: Color32::from_black_alpha(40),
};

fn visuals(p: &Palette, dark: bool) -> Visuals {
    let mut v = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    v.panel_fill = p.window;
    v.window_fill = p.card;
    v.faint_bg_color = p.card;
    v.extreme_bg_color = p.view;
    v.code_bg_color = p.view;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.selection.bg_fill = p.selection;
    v.selection.stroke = Stroke::new(1.0, p.accent_text);
    v.hyperlink_color = p.accent_text;
    v.weak_text_color = Some(p.weak_text);
    v.override_text_color = None;
    v.window_shadow = Shadow {
        offset: [0, 6],
        blur: 22,
        spread: 0,
        color: p.shadow,
    };
    v.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 14,
        spread: 0,
        color: p.shadow,
    };
    v.text_cursor.stroke = Stroke::new(2.0, p.accent_text);
    let w = &mut v.widgets;
    // Flat controls: no outline, a fill that brightens with interaction.
    w.noninteractive.bg_fill = p.window;
    w.noninteractive.weak_bg_fill = p.card;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    for (state, fill) in [
        (&mut w.inactive, p.control),
        (&mut w.hovered, p.hover),
        (&mut w.active, p.pressed),
        (&mut w.open, p.hover),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.bg_stroke = Stroke::NONE;
        state.fg_stroke = Stroke::new(1.0, p.text);
        state.expansion = 0.0;
    }
    // Focus and the pressed state still show where the accent belongs.
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    v
}

/// The header band's color (menus, toolbar, status bar, tab bars).
pub fn header(visuals: &Visuals) -> Color32 {
    if visuals.dark_mode {
        DARK.header
    } else {
        LIGHT.header
    }
}

/// The accent in the theme of `visuals`.
pub fn accent(visuals: &Visuals) -> Color32 {
    if visuals.dark_mode {
        DARK.accent
    } else {
        LIGHT.accent
    }
}

/// Installs both themes and selects `choice`.
/// The touch layout's sizes (docs/07 §13.1): controls at least 40 points
/// high, more room between them; `false` restores the usual sizes.
pub fn set_touch(ctx: &egui::Context, on: bool) {
    ctx.all_styles_mut(|style| {
        let s = &mut style.spacing;
        if on {
            s.item_spacing = egui::vec2(10.0, 10.0);
            s.button_padding = egui::vec2(14.0, 10.0);
            s.interact_size.y = 40.0;
            s.scroll.bar_width = 14.0;
        } else {
            s.item_spacing = egui::vec2(8.0, 6.0);
            s.button_padding = egui::vec2(10.0, 4.0);
            s.interact_size.y = 26.0;
            s.scroll.bar_width = egui::style::ScrollStyle::default().bar_width;
        }
    });
}

pub fn apply(ctx: &egui::Context, choice: ThemeChoice) {
    ctx.set_visuals_of(Theme::Light, visuals(&LIGHT, false));
    ctx.set_visuals_of(Theme::Dark, visuals(&DARK, true));
    ctx.all_styles_mut(|style| {
        let radius = CornerRadius::same(6);
        let v = &mut style.visuals;
        v.window_corner_radius = CornerRadius::same(12);
        v.menu_corner_radius = CornerRadius::same(8);
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
        s.item_spacing = egui::vec2(8.0, 6.0);
        s.button_padding = egui::vec2(10.0, 4.0);
        s.menu_margin = Margin::same(6);
        s.window_margin = Margin::same(12);
        s.interact_size.y = 26.0;
        s.combo_height = 260.0;
    });
    ctx.set_theme(match choice {
        ThemeChoice::System => ThemePreference::System,
        ThemeChoice::Light => ThemePreference::Light,
        ThemeChoice::Dark => ThemePreference::Dark,
    });
}
