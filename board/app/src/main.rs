//! Native entry point.
//!
//! The browser build has no `main`: there the page calls into `web.rs`.
//! Keeping both in one crate is the point - one board, two ways to run it.

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result<()> {
    env_logger::init();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1400.0, 860.0])
            .with_title("tradebot board"),
        ..Default::default()
    };
    eframe::run_native(
        "tradebot board",
        options,
        Box::new(|cc| {
            board::theme::apply(&cc.egui_ctx);
            let mut board = board::app::Board::default();
            if let Some(saved) = board::store::load() {
                board.restore(&saved);
            }
            Ok(Box::new(board))
        }),
    )
}

#[cfg(target_arch = "wasm32")]
fn main() {}
