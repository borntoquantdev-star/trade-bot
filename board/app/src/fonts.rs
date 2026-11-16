//! The Thai font, bundled into the binary.
//!
//! egui's built-in fonts (Ubuntu-Light and Hack) contain **no Thai glyphs at
//! all** - zero of the 87 in the block. Every Thai label on this board was
//! therefore being drawn as nothing: the numbers and the latin words showed
//! up, the sentences around them did not. A screenshot of the chart area
//! looks fine, which is exactly how this survived a visual check.
//!
//! So the font travels with the binary. Not loaded from the page, not
//! fetched at startup: the board has to open on a machine with no network
//! beyond the exchange, and a missing font is not a thing a student can fix.
//!
//! Noto Sans Thai Regular, SIL Open Font License 1.1 - see
//! `assets/OFL.txt`. It is Thai-only (37 KB), which is all that is wanted:
//! it goes in as a *fallback*, so latin text, digits and the middot keep
//! coming from egui's own fonts and only Thai reaches for this one.

use egui::{FontData, FontFamily, FontInsert, FontPriority, InsertFontFamily};

const THAI: &[u8] = include_bytes!("../assets/NotoSansThai-Regular.ttf");

const NAME: &str = "noto-sans-thai";

/// Add Thai to both font families. Call once, before the first frame.
pub fn install(ctx: &egui::Context) {
    ctx.add_font(FontInsert::new(
        NAME,
        FontData::from_static(THAI),
        vec![
            InsertFontFamily {
                family: FontFamily::Proportional,
                // Lowest = fallback. Highest would hand latin text to a Thai
                // font as well, and the board would change shape for no
                // reason.
                priority: FontPriority::Lowest,
            },
            // Monospace is not only for numbers here: the market's own notes
            // are printed in it.
            InsertFontFamily {
                family: FontFamily::Monospace,
                priority: FontPriority::Lowest,
            },
        ],
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Thai from every card, so a label added later that uses a character
    /// this font happens to lack cannot slip through.
    const LABELS: [&str; 10] = [
        "ทุนกับสถานะบอท",
        "ทางที่การตัดสินใจเดิน",
        "สิ่งที่บอทตัดสินใจ",
        "ผลการทดสอบย้อนหลัง",
        "ความเสี่ยงกับคุณภาพไม้",
        "เส้นทุน · แรเงาคือช่วงใต้พีค",
        "อ่านก่อนเชื่อผลนี้",
        "วัดไม่ได้ · ปฏิเสธ · ซื้อแล้วถือ · ค่าธรรมเนียม",
        "หน้าตากลยุทธ์ · มันดูเส้นอะไรอยู่ · เส้นกลาง · แถบที่ไม่ทำอะไร",
        "รอจับคู่ · ถือ ซื้อ · stop · ตัดขาดทุนที่นี่ · ยังไม่ปิด",
    ];

    /// Every non-ASCII symbol the cards print. A character with no glyph is
    /// drawn as an empty box, and an empty box next to a number is worse
    /// than no symbol at all.
    const SYMBOLS: &str = "· ÷ — % × ‹ ›";

    fn with_fonts(install_ours: bool) -> egui::Context {
        let ctx = egui::Context::default();
        if install_ours {
            install(&ctx);
        }
        // Fonts are built on the first pass, not on construction. The pass
        // hands back a texture delta that panics if it is dropped unapplied,
        // and there is no painter here to apply it to.
        let mut output = ctx.run_ui(Default::default(), |_| {});
        output.textures_delta.clear();
        ctx
    }

    /// Thai is not the only thing that can come out as a box. An arrow or a
    /// dash the board prints between two numbers has to exist in one of the
    /// fonts, and `→` did not: it reached the screen as an empty square
    /// between the opening and closing equity.
    #[test]
    fn every_symbol_the_cards_print_has_a_glyph_too() {
        let ctx = with_fonts(true);
        ctx.fonts_mut(|fonts| {
            for font in [
                egui::FontId::proportional(12.0),
                egui::FontId::monospace(12.0),
            ] {
                for symbol in SYMBOLS.split_whitespace() {
                    assert!(
                        fonts.has_glyphs(&font, symbol),
                        "ไม่มีรูปอักขระสำหรับ {symbol:?} ใน {font:?} — มันจะขึ้นจอเป็นสี่เหลี่ยมโหว่"
                    );
                }
            }
        });
    }

    /// A loss is a number with a minus in front of it, and the minus is the
    /// only thing distinguishing it from a win.
    ///
    /// This exists because one went missing on screen: the closed-trade card
    /// printed a loss of 17.79 as `17.79`, in the same column where a win
    /// printed as `+2.14`. A sign that silently does not draw is the worst
    /// possible failure on a money card - it does not look broken, it looks
    /// like profit.
    #[test]
    fn a_minus_sign_survives_being_drawn_in_every_font_the_cards_use() {
        let ctx = with_fonts(true);
        ctx.fonts_mut(|fonts| {
            for font in [
                egui::FontId::proportional(10.0),
                egui::FontId::proportional(12.0),
                egui::FontId::monospace(10.0),
                egui::FontId::monospace(12.0),
            ] {
                for text in ["-", "-17.79", "+2.14"] {
                    assert!(
                        fonts.has_glyphs(&font, text),
                        "ไม่มีรูปอักขระสำหรับ {text:?} ใน {font:?}"
                    );
                }
                // Laid out, not just present: a glyph that exists but has no
                // width leaves the same hole on screen.
                let loss =
                    fonts.layout_no_wrap("-17.79".to_owned(), font.clone(), egui::Color32::WHITE);
                let win =
                    fonts.layout_no_wrap("17.79".to_owned(), font.clone(), egui::Color32::WHITE);
                assert!(
                    loss.size().x > win.size().x,
                    "เครื่องหมายลบไม่กินที่เลยใน {font:?}: {} เทียบกับ {}",
                    loss.size().x,
                    win.size().x
                );
            }
        });
    }

    #[test]
    fn every_thai_label_on_the_board_has_a_glyph_to_draw() {
        let ctx = with_fonts(true);
        ctx.fonts_mut(|fonts| {
            for label in LABELS {
                for font in [
                    egui::FontId::proportional(12.0),
                    egui::FontId::monospace(12.0),
                ] {
                    assert!(
                        fonts.has_glyphs(&font, label),
                        "ไม่มีรูปอักขระสำหรับ {label:?} ใน {font:?}"
                    );
                }
            }
        });
    }

    /// The test above only means something if the board would really fail
    /// without the bundled font. It would: egui's own fonts carry no Thai at
    /// all, which is how a board full of invisible sentences passed a look.
    #[test]
    fn without_the_bundled_font_those_labels_would_draw_as_nothing() {
        let ctx = with_fonts(false);
        ctx.fonts_mut(|fonts| {
            let font = egui::FontId::proportional(12.0);
            assert!(
                !fonts.has_glyphs(&font, LABELS[0]),
                "ถ้าฟอนต์เดิมมีสระไทยแล้ว เทสข้างบนก็ไม่ได้พิสูจน์อะไร"
            );
            // Latin and the middot keep working either way: this font is a
            // fallback, not a replacement.
            assert!(fonts.has_glyphs(&font, "risk_gate() · sharpe 1.35"));
        });
    }

    /// A font that failed to download is still a file. The first attempt at
    /// fetching this one wrote 14 bytes of `404: Not Found` into the asset
    /// path, and nothing but a test like this would have noticed before the
    /// board shipped with invisible text again.
    #[test]
    fn the_bundled_font_is_a_real_font_file() {
        assert!(
            THAI.len() > 20_000,
            "ฟอนต์เล็กเกินกว่าจะเป็นฟอนต์จริง: {} ไบต์",
            THAI.len()
        );
        let magic = &THAI[0..4];
        assert!(
            magic == [0x00, 0x01, 0x00, 0x00] || magic == b"true" || magic == b"OTTO",
            "ไฟล์นี้ไม่ได้ขึ้นต้นแบบฟอนต์: {magic:?}"
        );
    }
}
