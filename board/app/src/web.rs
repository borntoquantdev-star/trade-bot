//! Browser entry point.
//!
//! This is the build students get: the Docker image serves the WASM bundle and
//! the page opens it. Everything else in the crate is shared with the native
//! build, so there is no second implementation of anything here.
//!
//! Trunk injects its own loader and calls the `start` function below once the
//! bundle is ready, so the page needs no script of its own - which also means
//! nothing in the HTML has to know the hashed bundle filename.

use wasm_bindgen::prelude::*;

const CANVAS_ID: &str = "board_canvas";
const BOOT_ID: &str = "boot";

#[wasm_bindgen(start)]
pub fn start() {
    // Without this, a panic is a blank canvas and no explanation.
    console_error_panic_hook::set_once();

    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };

    let canvas = document
        .get_element_by_id(CANVAS_ID)
        .and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok());

    let Some(canvas) = canvas else {
        fail(&document, &format!("ไม่พบ canvas ชื่อ {CANVAS_ID}"));
        return;
    };

    wasm_bindgen_futures::spawn_local(async move {
        let result = eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(|cc| {
                    crate::theme::apply(&cc.egui_ctx);
                    let mut board = crate::app::Board::default();
                    if let Some(saved) = crate::store::load() {
                        board.restore(&saved);
                    }
                    // `/#bot` opens on the pipeline, `/#history` on the
                    // runs - so a worksheet can link to the zone it means.
                    let hash = web_sys::window()
                        .and_then(|w| w.location().hash().ok())
                        .unwrap_or_default();
                    if !hash.is_empty() {
                        board.open_at(&hash);
                    }
                    Ok(Box::new(board))
                }),
            )
            .await;

        let Some(document) = web_sys::window().and_then(|w| w.document()) else {
            return;
        };
        match result {
            Ok(()) => {
                if let Some(boot) = document.get_element_by_id(BOOT_ID) {
                    boot.remove();
                }
            }
            Err(err) => fail(&document, &format!("{err:?}")),
        }
    });
}

/// Replace the loading splash with something that says what went wrong.
/// A board that fails silently teaches a student nothing.
fn fail(document: &web_sys::Document, message: &str) {
    if let Some(boot) = document.get_element_by_id(BOOT_ID) {
        boot.set_inner_html(&format!(
            "<b>เปิดบอร์ดไม่ได้</b><small>{}</small>",
            html_escape(&message.chars().take(300).collect::<String>())
        ));
    }
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
