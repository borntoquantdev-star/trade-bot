//! What the account has to do with the chart: the lines and the triangles
//! drawn over the candles.
//!
//! Worked out here, away from the drawing, so the question "which line is
//! that and where did it come from" has one answer that a test can read.
//!
//! **The exchange first, the bot's own notes second.** With a key the board
//! can see the orders still in the book, the position and every fill as the
//! exchange holds them, and those are the truth. Without one, the bot's audit
//! trail still says what it sent and what it held, which is worth drawing as
//! long as the label says whose number it is.

use crate::account::{Account, Kind};
use crate::bot::Row;
use crate::candles::{Level, Load, Mark, Overlay};
use crate::theme;

/// How fast a waiting order's line breathes, in cycles a second. Slow
/// enough to read the label, fast enough to say "this is still moving".
const BREATH: f64 = 1.2;

/// The lines and triangles for `instrument` right now.
pub fn overlay(instrument: &str, account: &Account, rows: &[Row], now: f64) -> Overlay {
    let mine: Vec<&Row> = rows.iter().filter(|row| row.symbol == instrument).collect();
    let exchange = account.has_key();
    let mut over = Overlay::default();

    // Orders still waiting in the book. Only the exchange knows these: the
    // bot's trail says an order went, not whether it is still there.
    let breath = (0.6 + 0.4 * (now * BREATH * std::f64::consts::TAU).sin()) as f32;
    for order in account
        .open_orders
        .iter()
        .filter(|o| o.instrument == instrument)
    {
        let Some(price) = order.price.as_ref().map(|p| p.value).filter(|p| *p > 0.0) else {
            continue;
        };
        let size = order
            .size
            .as_ref()
            .map(|size| format!(" {}", size.text))
            .unwrap_or_default();
        over.levels.push(Level {
            price,
            colour: theme::ACCENT.gamma_multiply(breath),
            dashed: true,
            label: format!("รอจับคู่ · {}{size}", order.side()),
        });
    }

    // The position, and where it was opened.
    let held = if exchange && matches!(account.load_of(Kind::Positions), Load::Ready) {
        account
            .positions
            .iter()
            .find(|p| p.instrument == instrument && p.size.as_ref().is_some_and(|s| s.value != 0.0))
            .and_then(|p| {
                let entry = p.entry.as_ref()?.value;
                let size = p.size.as_ref()?.text.trim_start_matches('-').to_string();
                let open = p
                    .unrealized
                    .as_ref()
                    .map(|u| format!(" · ยังไม่ปิด {:+.2}", u.value))
                    .unwrap_or_default();
                Some((p.is_long(), entry, format!("ถือ {} {size}{open}", p.side())))
            })
    } else {
        // No key: the bot's newest row says what it held, and says so.
        mine.last().and_then(|row| {
            let position = row.position.as_ref()?;
            let entry: f64 = position.entry_price.trim().parse().ok()?;
            let long = position.side == "buy";
            let side = if long { "ซื้อ" } else { "ขาย" };
            Some((
                long,
                entry,
                format!("ถือ {side} {} (บันทึกของบอท)", position.qty),
            ))
        })
    };
    if let Some((long, entry, label)) = held {
        over.levels.push(Level {
            price: entry,
            colour: if long { theme::OK } else { theme::BAD },
            dashed: false,
            label,
        });
        if let Some(stop) = stop_of(&mine) {
            over.levels.push(Level {
                price: stop,
                colour: theme::BAD,
                dashed: true,
                label: "stop · ตัดขาดทุนที่นี่".to_string(),
            });
        }
    }

    // Where it traded.
    if exchange && matches!(account.load_of(Kind::Fills), Load::Ready) {
        for fill in account.fills.iter().filter(|f| f.instrument == instrument) {
            let (Some(unix), Some(price)) = (fill.when, fill.price.as_ref()) else {
                continue;
            };
            over.marks.push(Mark {
                unix,
                price: price.value,
                is_buy: fill.is_buy,
            });
        }
    } else {
        over.marks
            .extend(mine.iter().filter_map(|row| bot_mark(row)));
    }
    over
}

/// The stop of the position the bot is in, as its strategy last stated it.
///
/// The strategy's own picture first, because it is worked out from the
/// position actually held. Failing that, the stop on the signal that opened
/// it - the newest one that became an order.
fn stop_of(rows: &[&Row]) -> Option<f64> {
    let newest = rows.last()?;
    newest.position.as_ref()?;
    if let Some(stop) = newest.level("stop") {
        return Some(stop);
    }
    rows.iter()
        .rev()
        .find(|row| row.order.is_some() && row.receipt.is_some())
        .and_then(|row| row.signal.as_ref())
        .and_then(|signal| signal.stop_price.trim().parse().ok())
}

/// A trade the bot's trail says it made: an order that the exchange took,
/// or a close.
fn bot_mark(row: &Row) -> Option<Mark> {
    let unix = crate::time::unix_of(&row.at)?;
    if let (Some(order), Some(_)) = (&row.order, &row.receipt) {
        return Some(Mark {
            unix,
            price: order.limit_price.trim().parse().ok()?,
            is_buy: order.side == "buy",
        });
    }
    let exit = row.exit.as_ref()?;
    if exit.broker_order_id.is_empty() && !exit.simulated {
        // Asked to close, and nothing went out.
        return None;
    }
    let held = row.position.as_ref()?;
    Some(Mark {
        unix,
        price: row.price_value()?,
        is_buy: held.side != "buy",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account;

    fn row(json: &str) -> Row {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn without_a_key_the_bot_s_own_position_and_stop_are_drawn_and_say_so() {
        let rows = vec![
            row(
                r#"{"at":"2026-11-16T04:00:00+00:00","symbol":"BTC_USDT_Perp","price":"100",
                "signal":{"side":"buy","stop_price":"99.6"},
                "order":{"side":"buy","limit_price":"100.1","qty":"0.1"},
                "receipt":{"broker_order_id":"0x1"}}"#,
            ),
            row(
                r#"{"at":"2026-11-16T04:00:05+00:00","symbol":"BTC_USDT_Perp","price":"100.2",
                "position":{"side":"buy","qty":"0.1","entry_price":"100.05"}}"#,
            ),
        ];
        let over = overlay("BTC_USDT_Perp", &Account::default(), &rows, 0.0);
        let entry = &over.levels[0];
        assert_eq!(entry.price, 100.05);
        assert!(!entry.dashed, "ราคาเข้าคือราคาที่เกิดแล้ว เส้นทึบ");
        assert!(entry.label.contains("บันทึกของบอท"), "{}", entry.label);
        assert_eq!(over.levels[1].price, 99.6, "stop จากสัญญาณที่เปิดไม้");
        assert_eq!(over.marks.len(), 1);
        assert!(over.marks[0].is_buy);
        assert_eq!(over.marks[0].price, 100.1);
    }

    #[test]
    fn the_strategy_s_own_stop_wins_over_the_signal_s() {
        let rows = vec![
            row(
                r#"{"at":"2026-11-16T04:00:00+00:00","symbol":"BTC_USDT_Perp","price":"100",
                "signal":{"side":"buy","stop_price":"99.6"},
                "order":{"side":"buy","limit_price":"100.1"},"receipt":{"broker_order_id":"0x1"}}"#,
            ),
            row(
                r#"{"at":"2026-11-16T04:00:05+00:00","symbol":"BTC_USDT_Perp","price":"100",
                "position":{"side":"buy","qty":"0.1","entry_price":"100.2"},
                "view":{"stop":"99.7992"}}"#,
            ),
        ];
        let over = overlay("BTC_USDT_Perp", &Account::default(), &rows, 0.0);
        assert_eq!(over.levels[1].price, 99.7992);
    }

    #[test]
    fn a_flat_bot_draws_no_entry_and_no_stop() {
        let rows = vec![row(
            r#"{"at":"2026-11-16T04:00:00+00:00","symbol":"BTC_USDT_Perp","price":"100","view":{"stop":"1"}}"#,
        )];
        let over = overlay("BTC_USDT_Perp", &Account::default(), &rows, 0.0);
        assert!(over.levels.is_empty(), "{:?}", over.levels);
    }

    #[test]
    fn a_close_is_drawn_on_the_other_side_of_the_position() {
        let rows = vec![row(
            r#"{"at":"2026-11-16T04:00:00+00:00","symbol":"BTC_USDT_Perp","price":"101",
                "position":{"side":"buy","qty":"0.1","entry_price":"100"},
                "exit":{"reason":"came back","broker_order_id":"0x00"}}"#,
        )];
        let over = overlay("BTC_USDT_Perp", &Account::default(), &rows, 0.0);
        assert_eq!(over.marks.len(), 1);
        assert!(!over.marks[0].is_buy, "ปิด long คือขาย");
        assert_eq!(over.marks[0].price, 101.0);
    }

    #[test]
    fn another_contract_s_rows_are_not_drawn_on_this_chart() {
        let rows = vec![row(
            r#"{"at":"2026-11-16T04:00:00+00:00","symbol":"ETH_USDT_Perp","price":"3000",
                "position":{"side":"buy","qty":"1","entry_price":"3000"}}"#,
        )];
        let over = overlay("BTC_USDT_Perp", &Account::default(), &rows, 0.0);
        assert!(over.levels.is_empty() && over.marks.is_empty());
    }

    #[test]
    fn an_order_waiting_in_the_book_is_a_dashed_accent_line_at_its_price() {
        let mut account = Account::default();
        account.open_orders = vec![account::Order {
            instrument: "BTC_USDT_Perp".into(),
            is_buy: true,
            price: crate::exchange::Num::from(Some(&serde_json::json!("67000.5"))),
            size: crate::exchange::Num::from(Some(&serde_json::json!("0.01"))),
            ..Default::default()
        }];
        let over = overlay("BTC_USDT_Perp", &account, &[], 0.0);
        assert_eq!(over.levels.len(), 1);
        assert_eq!(over.levels[0].price, 67000.5);
        assert!(over.levels[0].dashed);
        assert!(
            over.levels[0].label.starts_with("รอจับคู่"),
            "{}",
            over.levels[0].label
        );
        assert!(over.levels[0].label.contains("0.01"));
    }
}
