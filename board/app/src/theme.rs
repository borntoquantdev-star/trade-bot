//! The board's palette, in one place.
//!
//! Same language as the web dashboard in `dashboard/`: a warm near-black
//! canvas, cream text, hairline borders.
//!
//! **Orange means "system", never "money."** The selected tab, the held tool,
//! a wire with data moving through it. Profit and loss get green, amber and
//! red on a separate scale. Mixing them would put the colour for "this is
//! selected" next to the colour for "this lost money", which is the one
//! confusion a trading screen cannot afford.

use egui::{Color32, CornerRadius, Stroke};

pub const CANVAS: Color32 = Color32::from_rgb(0x14, 0x11, 0x0e);
pub const GRID: Color32 = Color32::from_rgb(0x22, 0x1b, 0x15);
pub const SURFACE: Color32 = Color32::from_rgb(0x1a, 0x16, 0x12);
pub const HEAD: Color32 = Color32::from_rgb(0x21, 0x1b, 0x16);

pub const TEXT: Color32 = Color32::from_rgb(0xf1, 0xe9, 0xdd);
pub const MUTED: Color32 = Color32::from_rgb(0xcd, 0xc2, 0xb3);
pub const DIM: Color32 = Color32::from_rgb(0x8b, 0x7e, 0x72);
pub const FAINT: Color32 = Color32::from_rgb(0x6b, 0x60, 0x56);

pub const ACCENT: Color32 = Color32::from_rgb(0xff, 0x6b, 0x3d);

/// A wire with nothing moving through it. Warm grey, the same `--wire` the
/// web dashboard uses, so the two boards read as one family.
pub const WIRE: Color32 = Color32::from_rgb(0x6b, 0x54, 0x44);

pub const OK: Color32 = Color32::from_rgb(0x5f, 0xbe, 0x86);
pub const WARN: Color32 = Color32::from_rgb(0xe8, 0xa3, 0x3d);
pub const BAD: Color32 = Color32::from_rgb(0xf0, 0x56, 0x4d);
pub const INFO: Color32 = Color32::from_rgb(0x6a, 0xa7, 0xff);

/// Hairline border, the same tenth-of-an-alpha as the web dashboard.
pub fn line() -> Stroke {
    Stroke::new(1.0, Color32::from_rgba_unmultiplied(0xf1, 0xe9, 0xdd, 26))
}

pub fn card_radius() -> CornerRadius {
    CornerRadius::same(12)
}

pub fn chip_radius() -> CornerRadius {
    CornerRadius::same(99)
}

/// How a card's state colours its edge and its status chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Nothing has happened yet.
    Idle,
    /// Working, or a number that is information rather than a verdict.
    Wip,
    /// Good.
    Ok,
    /// Bad: refused, halted, lost money.
    Bad,
    /// Simulated rather than real.
    Simulated,
}

impl State {
    pub fn colour(self) -> Color32 {
        match self {
            State::Idle => FAINT,
            State::Wip => WARN,
            State::Ok => OK,
            State::Bad => BAD,
            State::Simulated => INFO,
        }
    }

    /// The card's border. Idle stays on the hairline so a board full of
    /// waiting cards does not look like a board full of alarms.
    pub fn border(self) -> Stroke {
        match self {
            State::Idle => line(),
            other => Stroke::new(1.0, other.colour().gamma_multiply(0.55)),
        }
    }

    pub fn head_fill(self) -> Color32 {
        match self {
            State::Idle => HEAD,
            State::Wip => Color32::from_rgb(0x25, 0x1c, 0x10),
            State::Ok => Color32::from_rgb(0x15, 0x24, 0x1c),
            State::Bad => Color32::from_rgb(0x27, 0x13, 0x10),
            State::Simulated => Color32::from_rgb(0x14, 0x1c, 0x27),
        }
    }
}

/// Apply the palette to egui's own widgets so menus and scrollbars match.
pub fn apply(ctx: &egui::Context) {
    // Thai first: every label below is written in it.
    crate::fonts::install(ctx);

    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = CANVAS;
    visuals.window_fill = SURFACE;
    visuals.extreme_bg_color = Color32::from_rgb(0x0f, 0x0c, 0x09);
    visuals.override_text_color = Some(TEXT);
    visuals.widgets.noninteractive.bg_stroke = line();
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(0x25, 0x1e, 0x18);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(0x2e, 0x25, 0x1d);
    visuals.widgets.active.bg_fill = ACCENT;
    visuals.selection.bg_fill = ACCENT.gamma_multiply(0.35);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    ctx.set_visuals(visuals);
}
