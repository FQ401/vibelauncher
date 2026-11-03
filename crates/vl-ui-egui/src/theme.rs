//! Design tokens as egui visuals and style.

use egui::{Color32, Context, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Theme, Visuals};

/// Light or dark appearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeMode {
    /// The default.
    #[default]
    Dark,
    /// The light theme.
    Light,
}

impl ThemeMode {
    fn theme(self) -> Theme {
        match self {
            ThemeMode::Dark => Theme::Dark,
            ThemeMode::Light => Theme::Light,
        }
    }
}

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// The color tokens of one theme.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub surface: Color32,
    pub surface_raised: Color32,
    pub surface_sunken: Color32,
    pub surface_hover: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    pub accent: Color32,
    pub accent_solid: Color32,
    pub accent_hover: Color32,
    pub on_accent: Color32,
    pub accent_subtle: Color32,
    pub selection: Color32,
    pub success: Color32,
    pub danger: Color32,
}

impl Palette {
    /// The dark theme: an inversion of the light one (`#1B1B1D` … `#F2F2F2`).
    pub const DARK: Palette = Palette {
        bg: hex(0x1B1B1D),
        surface: hex(0x242426),
        surface_raised: hex(0x242426),
        surface_sunken: hex(0x323234),
        surface_hover: hex(0x323234),
        border: hex(0x4C4C4F),
        border_strong: hex(0xF2F2F2),
        text: hex(0xF2F2F2),
        text_muted: hex(0xA3A3A3),
        accent: hex(0xF2F2F2),
        accent_solid: hex(0xF2F2F2),
        accent_hover: hex(0xFFFFFF),
        on_accent: hex(0x1B1B1D),
        accent_subtle: hex(0x323234),
        selection: hex(0x48484B),
        success: hex(0x4ADE80),
        danger: hex(0xF87171),
    };

    /// The light theme: a monochrome scale (`#F2F2F2` … `#222222`), no hue.
    pub const LIGHT: Palette = Palette {
        bg: hex(0xF2F2F2),
        surface: hex(0xFFFFFF),
        surface_raised: hex(0xFFFFFF),
        surface_sunken: hex(0xE6E6E6),
        surface_hover: hex(0xE6E6E6),
        border: hex(0xBDBDBD),
        border_strong: hex(0x222222),
        text: hex(0x222222),
        text_muted: hex(0x6B6B6B),
        accent: hex(0x222222),
        accent_solid: hex(0x222222),
        accent_hover: hex(0x000000),
        on_accent: hex(0xFFFFFF),
        accent_subtle: hex(0xE6E6E6),
        selection: hex(0xE6E6E6),
        success: hex(0x166534),
        danger: hex(0xB91C1C),
    };
}

/// Corner radius tokens.
pub const RADIUS_SM: u8 = 10;
pub const RADIUS_LG: u8 = 24;

/// Installs the theme for `mode`: colors, spacing and type scale.
pub fn apply(ctx: &Context, mode: ThemeMode) {
    let p = match mode {
        ThemeMode::Dark => Palette::DARK,
        ThemeMode::Light => Palette::LIGHT,
    };
    let theme = mode.theme();
    ctx.set_theme(theme);
    ctx.style_mut_of(theme, |style| {
        let mut v = match mode {
            ThemeMode::Dark => Visuals::dark(),
            ThemeMode::Light => Visuals::light(),
        };
        v.panel_fill = p.bg;
        v.window_fill = p.surface_raised;
        v.window_stroke = Stroke::new(1.0, p.border);
        v.extreme_bg_color = p.surface_sunken;
        v.faint_bg_color = p.surface;
        v.hyperlink_color = p.accent;
        v.selection.bg_fill = p.selection;
        v.selection.stroke = Stroke::new(1.0, p.accent);
        let widgets = &mut v.widgets;
        widgets.noninteractive.bg_fill = p.surface;
        widgets.noninteractive.weak_bg_fill = p.surface;
        widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
        widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
        widgets.inactive.bg_fill = p.surface_raised;
        widgets.inactive.weak_bg_fill = p.surface_raised;
        widgets.inactive.bg_stroke = Stroke::new(1.0, p.border);
        widgets.inactive.fg_stroke = Stroke::new(1.0, p.text);
        widgets.hovered.bg_fill = p.surface_hover;
        widgets.hovered.weak_bg_fill = p.surface_hover;
        widgets.hovered.bg_stroke = Stroke::new(1.0, p.border_strong);
        widgets.hovered.fg_stroke = Stroke::new(1.5, p.text);
        widgets.active.bg_fill = p.accent_subtle;
        widgets.active.weak_bg_fill = p.accent_subtle;
        widgets.active.bg_stroke = Stroke::new(1.0, p.accent);
        widgets.active.fg_stroke = Stroke::new(1.5, p.text);
        widgets.open.bg_fill = p.surface_hover;
        widgets.open.weak_bg_fill = p.surface_hover;
        for w in [
            &mut widgets.noninteractive,
            &mut widgets.inactive,
            &mut widgets.hovered,
            &mut widgets.active,
            &mut widgets.open,
        ] {
            w.corner_radius = CornerRadius::same(RADIUS_SM);
        }
        style.visuals = v;
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.window_margin = egui::Margin::same(16);
        let sans = |size: f32| FontId::new(size, FontFamily::Proportional);
        style.text_styles = [
            (TextStyle::Small, sans(12.0)),
            (TextStyle::Body, sans(14.0)),
            (TextStyle::Button, sans(14.0)),
            (TextStyle::Heading, sans(22.0)),
            (
                TextStyle::Monospace,
                FontId::new(13.0, FontFamily::Monospace),
            ),
        ]
        .into();
    });
}
