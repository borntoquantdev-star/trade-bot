//! A card on the board: a framed panel with a draggable header.
//!
//! Cards live in *scene* coordinates, not screen coordinates. The scene
//! handles pan and zoom, so nothing here needs to know where the camera is -
//! which is the whole reason the board can be infinite.

use egui::{CornerRadius, FontId, Rect, Response, Sense, Ui, UiBuilder, Vec2};

use crate::theme;

/// Where a card sits and how big it is, in scene coordinates.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct Slot {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Slot {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn rect(self) -> Rect {
        Rect::from_min_size(egui::pos2(self.x, self.y), egui::vec2(self.w, self.h))
    }

    pub fn translate(&mut self, delta: Vec2) {
        self.x += delta.x;
        self.y += delta.y;
    }
}

pub const HEAD_HEIGHT: f32 = 30.0;

/// Draw one card. Returns the header's response so the caller can move it.
///
/// `status` is the short chip on the right of the header - the single most
/// load-bearing piece of text on the board, because it is what a person reads
/// when they glance at six cards at once.
pub fn show(
    ui: &mut Ui,
    slot: &mut Slot,
    title: &str,
    status: &str,
    state: theme::State,
    body: impl FnOnce(&mut Ui),
) -> Response {
    show_with_tools(ui, slot, title, status, state, |_| {}, body)
}

/// A card whose header carries buttons of its own, between the title and
/// the status chip.
///
/// The pickers that belong to one card live **on that card**: a top bar
/// that collects every control on the board turns into a row of chips that
/// say nothing about what they change.
pub fn show_with_tools(
    ui: &mut Ui,
    slot: &mut Slot,
    title: &str,
    status: &str,
    state: theme::State,
    tools: impl FnOnce(&mut Ui),
    body: impl FnOnce(&mut Ui),
) -> Response {
    let rect = slot.rect();
    let painter = ui.painter();

    // Frame
    painter.rect_filled(rect, theme::card_radius(), theme::SURFACE);

    // Header strip, with only its top corners rounded
    let head_rect = Rect::from_min_size(rect.min, egui::vec2(rect.width(), HEAD_HEIGHT));
    painter.rect_filled(
        head_rect,
        CornerRadius {
            nw: 12,
            ne: 12,
            sw: 0,
            se: 0,
        },
        state.head_fill(),
    );
    painter.hline(
        head_rect.x_range(),
        head_rect.max.y,
        egui::Stroke::new(1.0, state.border().color),
    );

    // Status dot
    painter.circle_filled(
        egui::pos2(head_rect.min.x + 12.0, head_rect.center().y),
        3.5,
        state.colour(),
    );

    // Title
    let title_galley =
        painter.layout_no_wrap(title.to_owned(), FontId::proportional(12.0), theme::TEXT);
    let title_width = title_galley.size().x;
    painter.galley(
        egui::pos2(
            head_rect.min.x + 23.0,
            head_rect.center().y - title_galley.size().y / 2.0,
        ),
        title_galley,
        theme::TEXT,
    );

    // Status chip on the right
    let mut chip_left = head_rect.max.x - 8.0;
    if !status.is_empty() {
        let font = FontId::proportional(10.0);
        let galley = ui
            .painter()
            .layout_no_wrap(status.to_owned(), font.clone(), state.colour());
        let chip = Rect::from_min_size(
            egui::pos2(
                head_rect.max.x - 8.0 - galley.size().x - 16.0,
                head_rect.center().y - galley.size().y / 2.0 - 3.0,
            ),
            galley.size() + egui::vec2(16.0, 6.0),
        );
        ui.painter().rect_filled(
            chip,
            theme::chip_radius(),
            state.colour().gamma_multiply(0.14),
        );
        ui.painter()
            .galley(chip.min + egui::vec2(8.0, 3.0), galley, state.colour());
        chip_left = chip.min.x;
    }

    // Body
    let body_rect = Rect::from_min_max(
        egui::pos2(rect.min.x + 12.0, rect.min.y + HEAD_HEIGHT + 10.0),
        egui::pos2(rect.max.x - 12.0, rect.max.y - 10.0),
    );
    if body_rect.width() > 1.0 && body_rect.height() > 1.0 {
        ui.scope_builder(UiBuilder::new().max_rect(body_rect), |ui| {
            ui.set_clip_rect(body_rect);
            body(ui);
        });
    }

    // Border last so it sits over the header's fill
    ui.painter().rect_stroke(
        rect,
        theme::card_radius(),
        state.border(),
        egui::StrokeKind::Inside,
    );

    // Dragging: the header only, same as the web dashboard. Registered
    // before the header's own buttons, so a click on a button is a click on
    // the button and not the start of a drag.
    let response = ui.interact(
        head_rect,
        ui.scope_id().with(title).with("head"),
        Sense::drag(),
    );
    if response.dragged() {
        slot.translate(response.drag_delta());
    }

    let tools_left =
        head_rect.min.x + 23.0 + title_width + if title.is_empty() { 0.0 } else { 10.0 };
    let tools_rect = Rect::from_min_max(
        egui::pos2(tools_left, head_rect.min.y + 2.0),
        egui::pos2(chip_left - 8.0, head_rect.max.y - 2.0),
    );
    if tools_rect.width() > 24.0 {
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(tools_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                ui.set_clip_rect(tools_rect);
                ui.spacing_mut().item_spacing.x = 3.0;
                tools(ui);
            },
        );
    }

    response
}

/// A label/value row for inside a card body.
pub fn kv(ui: &mut Ui, key: &str, value: &str, colour: egui::Color32) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(key).size(10.5).color(theme::DIM));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(value)
                    .size(12.5)
                    .monospace()
                    .color(colour),
            );
        });
    });
}
