//! The bot's own three files, turned into something a card can print.
//!
//! `logs/state.json`, `logs/audit.jsonl` and `logs/backtest.json` are written
//! by the Python bot and never by us. This module fetches them from the
//! board's server and derives the view: the five stages a decision walks
//! through, the account, the trail of what the bot did, and the backtest.
//!
//! It is the Rust twin of `dashboard/lib/pipeline.js` and
//! `dashboard/lib/backtest.js`, and deliberately reaches the same verdicts -
//! two screens over one bot that disagreed about whether it had traded would
//! be worse than having only one screen.
//!
//! Two rules carried over from those files, because they are the whole point:
//!
//! 1. **The numbers in those files are strings** - the bot writes `Decimal`
//!    at full precision. This module is where they become `f64`, and nowhere
//!    else.
//! 2. **A value that could not be measured is `None`, never `0.0`.** "no
//!    Sharpe ratio, there were no returns to measure" and "a Sharpe ratio of
//!    zero" are different sentences, and only one of them is true.

use std::collections::BTreeMap;
use std::sync::mpsc::{channel, Receiver, Sender};

use serde::Deserialize;

use crate::theme;

/// The same loading vocabulary the candle fetch uses, so the board has one
/// way of saying "not here yet" rather than two.
pub use crate::candles::Load;

/// How often each file is asked for again, in seconds. The audit trail and
/// the state move with the bot's loop; a backtest changes only when somebody
/// runs one, and the answer is the largest of the three.
const LIVE_EVERY: f64 = 2.0;
const BACKTEST_EVERY: f64 = 15.0;

/// Below this many trades a backtest is a story about luck. The bot itself
/// decides the threshold and writes it into the file as `thin_sample_below`;
/// this is only the fallback for a file written before that existed.
const THIN_FALLBACK: f64 = 30.0;

// ---------------------------------------------------------------- the files

#[derive(Debug, Clone, Default, Deserialize)]
pub struct BotState {
    #[serde(default)]
    pub day: String,
    #[serde(default)]
    pub day_start_equity: String,
    #[serde(default)]
    pub peak_equity: String,
    #[serde(default)]
    pub halted: bool,
    #[serde(default)]
    pub halt_reason: String,
    #[serde(default)]
    pub sent_order_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Row {
    #[serde(default)]
    pub at: String,
    #[serde(default)]
    pub symbol: String,
    #[serde(default)]
    pub price: String,
    #[serde(default)]
    pub equity: String,
    #[serde(default)]
    pub position: Option<Position>,
    #[serde(default)]
    pub signal: Option<Signal>,
    #[serde(default)]
    pub reject: Option<Reject>,
    #[serde(default)]
    pub order: Option<Order>,
    #[serde(default)]
    pub receipt: Option<Receipt>,
    /// The bot let a position go on this tick, and why.
    #[serde(default)]
    pub exit: Option<Exit>,
    #[serde(default)]
    pub error: Option<String>,
    /// Which lines of the bot's own code this tick ran. Absent from rows
    /// written before the bot could record it, and from a run that was told
    /// not to, which is why every reader here treats it as optional.
    #[serde(default)]
    pub trace: Option<Trace>,
    /// The server took this row's trace off for the trip and left this mark
    /// instead; the board asks for the trace when it shows the tick.
    #[serde(default)]
    pub traced: bool,
}

impl Row {
    /// Did the bot record which lines this tick ran - whether or not that
    /// record has arrived yet?
    pub fn has_trace(&self) -> bool {
        self.trace.is_some() || self.traced
    }
}

/// The path one tick took through the bot's code, as the bot recorded it.
///
/// Not a derivation from the rest of the row: `tradebot/trace.py` asks the
/// interpreter which lines it stepped through, so a card drawn from this is
/// showing what ran rather than what probably ran.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Trace {
    #[serde(default)]
    pub steps: u64,
    /// The functions entered, in the order they were first entered.
    #[serde(default)]
    pub calls: Vec<Call>,
    /// file -> line number -> how many times it ran. The numbers arrive as
    /// strings because they were JSON object keys.
    #[serde(default)]
    pub lines: BTreeMap<String, BTreeMap<String, u64>>,
    /// The files `path` refers to, in the order they were first entered.
    #[serde(default)]
    pub files: Vec<String>,
    /// Every step in order, as `(index into files, line number)`.
    ///
    /// The counts above say what a tick touched; this says what it *did*.
    /// Counting alone cannot tell a loop from a straight line through the
    /// same lines, and a card that walks needs the difference.
    #[serde(default)]
    pub path: Vec<(usize, usize)>,
    /// The recorder hit its ceiling and stopped keeping new lines.
    #[serde(default)]
    pub full: bool,
}

impl Trace {
    /// The steps this tick took inside one file, in order.
    ///
    /// Empty for a trail written before the bot recorded order, which is
    /// why nothing that reads this may assume it is there.
    pub fn steps_in(&self, file: &str) -> Vec<usize> {
        let Some(index) = self.files.iter().position(|name| name == file) else {
            return Vec::new();
        };
        self.path
            .iter()
            .filter(|(at, _)| *at == index)
            .map(|(_, number)| *number)
            .collect()
    }

    /// What ran in one file, by line number.
    ///
    /// A line number that will not parse is dropped rather than defaulted to
    /// zero: a phantom line 0 would light up the top of whatever function the
    /// card happened to be showing.
    pub fn hits(&self, file: &str) -> BTreeMap<usize, u64> {
        self.lines
            .get(file)
            .map(|counts| {
                counts
                    .iter()
                    .filter_map(|(number, count)| Some((number.parse().ok()?, *count)))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// One function the tick went into.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct Call {
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub func: String,
    /// The line the `def` sits on, which is how a function is named here.
    #[serde(default)]
    pub line: usize,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Exit {
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub broker_order_id: String,
    #[serde(default)]
    pub simulated: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Position {
    #[serde(default)]
    pub side: String,
    #[serde(default)]
    pub qty: String,
    #[serde(default)]
    pub entry_price: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Signal {
    #[serde(default)]
    pub side: String,
    #[serde(default)]
    pub ref_price: String,
    #[serde(default)]
    pub stop_price: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Reject {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Order {
    #[serde(default)]
    pub client_order_id: String,
    #[serde(default)]
    pub side: String,
    #[serde(default)]
    pub qty: String,
    #[serde(default)]
    pub limit_price: String,
    #[serde(default)]
    pub risk_amount: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Receipt {
    #[serde(default)]
    pub broker_order_id: String,
    #[serde(default)]
    pub simulated: bool,
}

// ------------------------------------------------------------- translations

/// Every refusal the gate can hand back, in the bot's own severity order.
/// The code is what the bot logged; the sentence is what a person reads.
pub const REJECTS: [(&str, &str); 14] = [
    ("halted", "บอทหยุดตัวเอง"),
    ("max_drawdown", "ชนเพดานขาดทุนจากจุดสูงสุด"),
    ("daily_loss_limit", "ชนเพดานขาดทุนรายวัน"),
    ("expired", "สัญญาณหมดอายุ"),
    ("short_not_allowed", "ตั้งห้าม short"),
    ("invalid_stop", "stop วางผิด"),
    ("slippage_exceeded", "ราคาวิ่งหนีเกินเพดาน"),
    ("already_in_position", "มีไม้ค้างอยู่แล้ว"),
    ("too_many_positions", "ไม้เต็มเพดาน"),
    ("qty_rounds_to_zero", "ทุนน้อยเกินไปสำหรับ stop นี้"),
    ("below_min_size", "ออเดอร์เล็กกว่าขนาดขั้นต่ำของตลาด"),
    ("below_min_notional", "ออเดอร์เล็กกว่าขั้นต่ำของตลาด"),
    ("exceeds_exposure", "เกินเพดาน exposure"),
    ("bad_instrument", "สเปคสินค้าหรือราคาใช้ไม่ได้"),
];

/// A code with no sentence is shown as the code. Inventing a translation for
/// a reason the bot grew after this file was written would be worse.
pub fn reject_label(code: &str) -> String {
    REJECTS
        .iter()
        .find(|(key, _)| *key == code)
        .map(|(_, label)| (*label).to_owned())
        .unwrap_or_else(|| code.to_owned())
}

/// Why a backtested trade ended, as `runner.py` recorded it.
fn exit_label(reason: &str) -> String {
    match reason {
        "stop" => "โดน stop".into(),
        "stop (gap)" => "โดน stop (เปิดกระโดดข้าม)".into(),
        "target" => "ถึงเป้า".into(),
        "target (gap)" => "ถึงเป้า (เปิดกระโดดข้าม)".into(),
        "still open at the end" => "ยังถืออยู่ตอนจบการทดสอบ".into(),
        other => other.into(),
    }
}

/// The limits that go on the screen every time, not in a footnote. The Thai
/// half of `report.caveats()`.
pub const NOTES: [&str; 4] = [
    "สัญลักษณ์เดียว ช่วงเดียว ค่าพารามิเตอร์ชุดเดียว เปลี่ยนอะไรก็เปลี่ยนเลข นั่นคือการทดสอบ ไม่ใช่ผลลัพธ์",
    "funding ของ perpetual ไม่ถูกหัก การถือข้ามวันจริงแพงกว่าที่เห็นนี้",
    "แบบจำลองการ fill ถือว่าออเดอร์เล็กพอที่จะไม่ขยับตลาด",
    "ขั้นต่อไปคือเทรดกระดาษ ไม่ใช่เงินจริง",
];

// ------------------------------------------------------------------ numbers

/// A number that has to have a value.
fn num(text: &str) -> f64 {
    maybe(text).unwrap_or(0.0)
}

/// A number that is allowed to be absent. Empty and unparsable both mean
/// "not measured", which is not the same as zero.
fn maybe(text: &str) -> Option<f64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    text.parse::<f64>().ok().filter(|v| v.is_finite())
}

fn maybe_json(value: Option<&serde_json::Value>) -> Option<f64> {
    match value? {
        serde_json::Value::String(text) => maybe(text),
        serde_json::Value::Number(number) => number.as_f64().filter(|v| v.is_finite()),
        _ => None,
    }
}

fn num_json(value: Option<&serde_json::Value>) -> f64 {
    maybe_json(value).unwrap_or(0.0)
}

fn text_json(value: Option<&serde_json::Value>) -> String {
    value
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_owned()
}

fn uint_json(value: Option<&serde_json::Value>) -> u64 {
    maybe_json(value).map(|v| v.max(0.0) as u64).unwrap_or(0)
}

/// The clock part of an ISO timestamp, in Thai time, which is all a row on
/// screen has room for. Returns the whole string if it does not look like
/// one, rather than cutting a sentence in half. See `time.rs` for why the
/// board converts at all.
pub fn clock(at: &str) -> String {
    crate::time::clock(at)
}

// ------------------------------------------------------------------- derive

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub ticks: usize,
    pub signals: usize,
    pub orders: usize,
    pub rejects: usize,
    pub exits: usize,
    pub errors: usize,
}

pub fn tally(rows: &[Row]) -> Counts {
    let mut out = Counts {
        ticks: rows.len(),
        ..Counts::default()
    };
    for row in rows {
        if row.signal.is_some() {
            out.signals += 1;
        }
        if row.order.is_some() {
            out.orders += 1;
        }
        if row.reject.is_some() {
            out.rejects += 1;
        }
        if row.exit.is_some() {
            out.exits += 1;
        }
        if row.error.is_some() {
            out.errors += 1;
        }
    }
    out
}

/// The newest row that managed to read a price. A row where the broker read
/// failed carries no price, and taking its zero would wipe the card.
fn last_with_price(rows: &[Row]) -> Option<&Row> {
    rows.iter().rev().find(|row| num(&row.price) > 0.0)
}

#[derive(Debug, Clone, Default)]
pub struct Account {
    pub equity: f64,
    pub day_start: f64,
    pub peak: f64,
    pub day_pnl: f64,
    pub day_pnl_pct: f64,
    pub drawdown_pct: f64,
    pub day: String,
    pub halted: bool,
    pub halt_reason: String,
    pub sent_orders: usize,
    pub symbol: String,
    pub updated_at: String,
}

pub fn account_of(rows: &[Row], state: Option<&BotState>) -> Account {
    let latest = last_with_price(rows);
    let day_start = state.map(|s| num(&s.day_start_equity)).unwrap_or(0.0);
    let equity = latest.map(|row| num(&row.equity)).unwrap_or(day_start);
    let peak = state.map(|s| num(&s.peak_equity)).unwrap_or(0.0);
    let day_pnl = if day_start > 0.0 {
        equity - day_start
    } else {
        0.0
    };
    Account {
        equity,
        day_start,
        peak,
        day_pnl,
        day_pnl_pct: if day_start > 0.0 {
            day_pnl / day_start * 100.0
        } else {
            0.0
        },
        drawdown_pct: if peak > 0.0 {
            (peak - equity) / peak * 100.0
        } else {
            0.0
        },
        day: state.map(|s| s.day.clone()).unwrap_or_default(),
        halted: state.map(|s| s.halted).unwrap_or(false),
        halt_reason: state.map(|s| s.halt_reason.clone()).unwrap_or_default(),
        sent_orders: state.map(|s| s.sent_order_ids.len()).unwrap_or(0),
        symbol: latest.map(|row| row.symbol.clone()).unwrap_or_default(),
        updated_at: rows.last().map(|row| row.at.clone()).unwrap_or_default(),
    }
}

/// One step of the road a decision walks down.
#[derive(Debug, Clone)]
pub struct Stage {
    pub label: &'static str,
    pub sub: &'static str,
    pub state: theme::State,
    pub metric: String,
    pub detail: String,
}

fn stage(label: &'static str, sub: &'static str) -> Stage {
    Stage {
        label,
        sub,
        state: theme::State::Idle,
        metric: String::new(),
        detail: "ยังไม่เริ่ม".into(),
    }
}

/// The five stages, in the order the data walks through them. A decision is
/// only ever as far along as the stage that stopped it, which is the thing
/// this card exists to show.
pub fn stages_of(rows: &[Row], state: Option<&BotState>) -> Vec<Stage> {
    let mut out = vec![
        stage("ข้อมูลตลาด", "ราคา · ทุน · position"),
        stage("กลยุทธ์", "ออกสัญญาณหรือเงียบ"),
        stage("ด่านความเสี่ยง", "risk_gate()"),
        stage("ส่งคำสั่ง", "broker"),
        stage("ไม้ที่ถืออยู่", "position"),
    ];
    let counts = tally(rows);
    let halted = state.map(|s| s.halted).unwrap_or(false);

    let Some(latest) = rows.last() else {
        out[0].detail = "รอบอทเขียน audit trail บรรทัดแรก".into();
        return out;
    };
    let market = last_with_price(rows);
    let error = latest.error.clone().unwrap_or_default();
    let lower = error.to_lowercase();

    // 1) market data
    let read_failed = lower.contains("broker read failed");
    if read_failed {
        out[0].state = theme::State::Bad;
        out[0].detail = error.clone();
    } else if let Some(row) = market {
        out[0].state = theme::State::Ok;
        out[0].metric = row.price.clone();
        out[0].detail = format!("{} · ทุน {}", row.symbol, row.equity);
    }

    // 2) strategy
    if lower.contains("strategy raised") {
        out[1].state = theme::State::Bad;
        out[1].detail = error.clone();
    } else if let Some(signal) = &latest.signal {
        out[1].state = theme::State::Ok;
        out[1].metric = signal.side.clone();
        out[1].detail = format!(
            "{} @ {} · stop {}",
            signal.side, signal.ref_price, signal.stop_price
        );
    } else if counts.ticks > 0 {
        out[1].state = theme::State::Wip;
        out[1].detail = format!("ยังไม่ออกสัญญาณ (ออกไปแล้ว {} ครั้ง)", counts.signals);
    }

    // 3) the gate
    if halted {
        let reason = state
            .map(|s| s.halt_reason.clone())
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| "ไม่ได้บันทึกเหตุผล".into());
        out[2].state = theme::State::Bad;
        out[2].detail = format!("หยุดอยู่: {reason}");
    } else if let Some(reject) = &latest.reject {
        out[2].state = theme::State::Bad;
        out[2].metric = reject.code.clone();
        out[2].detail = format!("{} — {}", reject_label(&reject.code), reject.detail);
    } else if let Some(order) = &latest.order {
        out[2].state = theme::State::Ok;
        out[2].metric = order.qty.clone();
        out[2].detail = format!(
            "อนุมัติ {} @ {} · เสี่ยง {}",
            order.qty, order.limit_price, order.risk_amount
        );
    } else if latest.signal.is_some() {
        out[2].state = theme::State::Wip;
        out[2].detail = "กำลังตรวจสัญญาณ".into();
    } else if counts.ticks > 0 {
        out[2].detail = format!("ไม่มีสัญญาณให้ตรวจ · ปฏิเสธไปแล้ว {} ครั้ง", counts.rejects);
    }

    // 4) the broker
    if !read_failed && lower.contains("broker") {
        out[3].state = theme::State::Bad;
        out[3].detail = error.clone();
    } else if let Some(receipt) = &latest.receipt {
        if receipt.simulated {
            out[3].state = theme::State::Simulated;
            out[3].metric = "paper".into();
            out[3].detail = format!("จำลอง (paper) · id {}", receipt.broker_order_id);
        } else {
            out[3].state = theme::State::Ok;
            out[3].metric = "live".into();
            out[3].detail = format!("ส่งจริงแล้ว · id {}", receipt.broker_order_id);
        }
    } else if latest.order.is_some() {
        out[3].state = theme::State::Wip;
        out[3].detail = "ส่งแล้วแต่ยังไม่มีใบยืนยัน".into();
    } else if counts.orders > 0 {
        out[3].detail = format!("ส่งไปแล้วทั้งหมด {} ออเดอร์", counts.orders);
    }

    // 5) the position
    if let Some(exit) = &latest.exit {
        // The tick that closed carries the position it closed, so saying
        // "holding" here would be the opposite of what just happened.
        out[4].state = theme::State::Wip;
        out[4].metric = "ปิดแล้ว".into();
        out[4].detail = format!("เพิ่งปิด: {}", exit.reason);
    } else if let Some(position) = &latest.position {
        out[4].state = theme::State::Ok;
        out[4].metric = position.side.clone();
        out[4].detail = format!(
            "{} {} @ {}",
            position.side, position.qty, position.entry_price
        );
    } else {
        out[4].detail = "ไม่มีไม้เปิดอยู่".into();
    }

    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Signal,
    Order,
    /// A position was let go of.
    Exit,
    Reject,
    Error,
}

impl Kind {
    pub fn colour(self) -> egui::Color32 {
        match self {
            Kind::Signal => theme::INFO,
            Kind::Order => theme::OK,
            Kind::Exit => theme::ACCENT,
            Kind::Reject => theme::WARN,
            Kind::Error => theme::BAD,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub at: String,
    pub kind: Kind,
    pub title: String,
    pub text: String,
}

/// What the bot did, newest first.
///
/// A tick where the bot looked at the market and did nothing is not an event:
/// it is the normal state of a bot that is behaving. Printing those would
/// bury the four lines that matter under a thousand that do not.
pub fn log_of(rows: &[Row], limit: usize) -> Vec<Entry> {
    let mut out = Vec::new();
    for row in rows.iter().rev() {
        if out.len() >= limit {
            break;
        }
        if let Some(error) = &row.error {
            out.push(Entry {
                at: row.at.clone(),
                kind: Kind::Error,
                title: "ผิดพลาด".into(),
                text: error.clone(),
            });
        } else if let Some(exit) = &row.exit {
            out.push(Entry {
                at: row.at.clone(),
                kind: Kind::Exit,
                title: "ปิดไม้".into(),
                text: if exit.simulated {
                    format!("{} · paper", exit.reason)
                } else {
                    exit.reason.clone()
                },
            });
        } else if let Some(order) = &row.order {
            let stamp = match &row.receipt {
                Some(receipt) if receipt.simulated => "paper",
                Some(_) => "live",
                None => "ไม่มีใบยืนยัน",
            };
            out.push(Entry {
                at: row.at.clone(),
                kind: Kind::Order,
                title: format!("{} {}", order.side, order.qty),
                text: format!(
                    "@ {} · เสี่ยง {} · {stamp}",
                    order.limit_price, order.risk_amount
                ),
            });
        } else if let Some(reject) = &row.reject {
            out.push(Entry {
                at: row.at.clone(),
                kind: Kind::Reject,
                title: reject_label(&reject.code),
                text: reject.detail.clone(),
            });
        } else if let Some(signal) = &row.signal {
            out.push(Entry {
                at: row.at.clone(),
                kind: Kind::Signal,
                title: format!("สัญญาณ {}", signal.side),
                text: signal.reason.clone(),
            });
        }
    }
    out
}

// ----------------------------------------------------------- the backtest

#[derive(Debug, Clone)]
pub struct Point {
    pub at: String,
    pub equity: f64,
}

#[derive(Debug, Clone)]
pub struct Trade {
    pub side: String,
    pub qty: Option<f64>,
    pub entry_at: String,
    pub entry_price: Option<f64>,
    pub exit_at: String,
    pub exit_price: Option<f64>,
    pub net_pnl: f64,
    pub r: Option<f64>,
    pub exit: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Winner {
    Bot,
    Hold,
}

#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub symbol: String,
    pub interval: String,
    pub strategy: String,
    pub exchange: String,
    pub bars: u64,
    pub start: String,
    pub end: String,

    pub start_equity: f64,
    pub final_equity: f64,
    pub net_pnl: f64,
    pub return_pct: f64,
    pub cagr_pct: Option<f64>,
    pub fees_paid: f64,

    pub hold_pct: Option<f64>,
    pub winner: Option<Winner>,

    pub max_drawdown: f64,
    pub max_drawdown_pct: f64,
    pub days_underwater: f64,
    pub worst_month: Option<(String, f64)>,
    pub losing_streak: u64,
    pub exposure_pct: f64,

    pub trades: u64,
    pub wins: u64,
    pub losses: u64,
    pub win_rate_pct: f64,
    pub win_loss_ratio: Option<f64>,
    pub profit_factor: Option<f64>,
    pub expectancy_r: f64,
    pub sharpe: Option<f64>,
    pub sortino: Option<f64>,
    pub without_best_two: f64,
    /// The run made money, and did not once the best two trades are removed.
    /// The result rests on two pieces of luck.
    pub rests_on_two: bool,

    pub signals: u64,
    /// (code, sentence, count), worst first
    pub rejects: Vec<(String, String, u64)>,
    pub reject_total: u64,
    pub unfilled: u64,
    pub halted: Option<(String, String)>,

    pub curve: Vec<Point>,
    pub curve_total: usize,
    pub trade_rows: Vec<Trade>,
    pub trades_hidden: usize,

    pub thin: bool,
    pub notes: Vec<String>,
    pub warnings: Vec<String>,
    /// Why there were no trades at all, in a sentence. Empty when there were.
    pub no_trades: String,
}

/// Build the view from one `/api/backtest` answer.
///
/// `data` is the bot's report with the two long lists already thinned by the
/// server; `curve_total` and `trades_total` are what they were before that,
/// because "600 points of 115,198" is a different claim from "600 points".
pub fn summary_of(data: &serde_json::Value, curve_total: usize, trades_total: usize) -> Summary {
    let get = |key: &str| data.get(key);
    let return_pct = num_json(get("return_pct"));
    let net_pnl = num_json(get("net_pnl"));
    let without_best_two = num_json(get("profit_without_best_two"));
    let hold_pct = maybe_json(get("buy_and_hold_pct"));
    let trades = uint_json(get("trades"));

    let mut rejects: Vec<(String, String, u64)> = data
        .get("rejects")
        .and_then(|v| v.as_object())
        .map(|map| {
            map.iter()
                .map(|(code, count)| (code.clone(), reject_label(code), uint_json(Some(count))))
                .collect()
        })
        .unwrap_or_default();
    rejects.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
    let reject_total = rejects.iter().map(|(_, _, count)| count).sum();

    let curve: Vec<Point> = data
        .get("equity_curve")
        .and_then(|v| v.as_array())
        .map(|points| {
            points
                .iter()
                .filter_map(|point| {
                    Some(Point {
                        at: text_json(point.get("at")),
                        equity: maybe_json(point.get("equity"))?,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    // Newest first, like the live log: the end of a run is what gets read.
    let trade_rows: Vec<Trade> = data
        .get("trade_list")
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter()
                .rev()
                .map(|trade| Trade {
                    side: text_json(trade.get("side")),
                    qty: maybe_json(trade.get("qty")),
                    entry_at: text_json(trade.get("entry_at")),
                    entry_price: maybe_json(trade.get("entry_price")),
                    exit_at: text_json(trade.get("exit_at")),
                    exit_price: maybe_json(trade.get("exit_price")),
                    net_pnl: num_json(trade.get("net_pnl")),
                    r: maybe_json(trade.get("r_multiple")),
                    exit: exit_label(&text_json(trade.get("exit_reason"))),
                })
                .collect()
        })
        .unwrap_or_default();

    let thin_below = maybe_json(get("thin_sample_below")).unwrap_or(THIN_FALLBACK);
    let thin = (trades as f64) < thin_below;

    let mut notes: Vec<String> = NOTES.iter().map(|note| (*note).to_string()).collect();
    if thin {
        notes.insert(
            0,
            format!(
                "{trades} ไม้น้อยเกินกว่าจะมีความหมาย ต่ำกว่าราวๆ {:.0} ไม้ โชคเสียงดังกว่าความได้เปรียบ",
                thin_below
            ),
        );
    }

    let halted_at = text_json(get("halted_at"));

    let mut summary = Summary {
        symbol: text_json(get("symbol")),
        interval: text_json(get("interval")),
        strategy: text_json(get("strategy")),
        exchange: text_json(get("exchange")),
        bars: uint_json(get("bars")),
        start: text_json(get("start")),
        end: text_json(get("end")),

        start_equity: num_json(get("starting_equity")),
        final_equity: num_json(get("final_equity")),
        net_pnl,
        return_pct,
        cagr_pct: maybe_json(get("cagr_pct")),
        fees_paid: num_json(get("fees_paid")),

        hold_pct,
        winner: hold_pct.map(|hold| {
            if return_pct > hold {
                Winner::Bot
            } else {
                Winner::Hold
            }
        }),

        max_drawdown: num_json(get("max_drawdown")),
        max_drawdown_pct: num_json(get("max_drawdown_pct")),
        days_underwater: num_json(get("days_underwater")),
        worst_month: data.get("worst_month").and_then(|month| {
            let name = text_json(month.get("month"));
            (!name.is_empty()).then(|| (name, num_json(month.get("return_pct"))))
        }),
        losing_streak: uint_json(get("longest_losing_streak")),
        exposure_pct: num_json(get("exposure_pct")),

        trades,
        wins: uint_json(get("wins")),
        losses: uint_json(get("losses")),
        win_rate_pct: num_json(get("win_rate_pct")),
        win_loss_ratio: maybe_json(get("win_loss_ratio")),
        profit_factor: maybe_json(get("profit_factor")),
        expectancy_r: num_json(get("expectancy_r")),
        sharpe: maybe_json(get("sharpe")),
        sortino: maybe_json(get("sortino")),
        without_best_two,
        rests_on_two: net_pnl > 0.0 && without_best_two <= 0.0,

        signals: uint_json(get("signals")),
        rejects,
        reject_total,
        unfilled: uint_json(get("unfilled_orders")),
        halted: (!halted_at.is_empty()).then(|| (halted_at, text_json(get("halt_reason")))),

        curve_total: curve_total.max(curve.len()),
        curve,
        trades_hidden: trades_total.saturating_sub(trade_rows.len()),
        trade_rows,

        thin,
        notes,
        warnings: data
            .get("warnings")
            .and_then(|v| v.as_array())
            .map(|list| {
                list.iter()
                    .map(|w| text_json(Some(w)))
                    .filter(|w| !w.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        no_trades: String::new(),
    };

    summary.no_trades = no_trade_sentence(&text_json(get("no_trade_reason")), &summary);
    summary
}

/// The bot decides *why* there were no trades; this only says it in Thai. The
/// ladder of conditions lives in Python, in one place.
fn no_trade_sentence(code: &str, summary: &Summary) -> String {
    match code {
        "" => String::new(),
        "no_candles" => "ไม่มีแท่งเทียนให้รัน".into(),
        "no_signal" => "กลยุทธ์ไม่ออกสัญญาณเลยบนข้อมูลชุดนี้".into(),
        "all_refused" => match summary.rejects.first() {
            Some((_, label, count)) => {
                format!("ทุกสัญญาณถูกด่านความเสี่ยงปฏิเสธ บ่อยสุดคือ {label} ({count} ครั้ง)")
            }
            None => "ทุกสัญญาณถูกด่านความเสี่ยงปฏิเสธ".into(),
        },
        "never_filled" => format!(
            "{} ออเดอร์ผ่านด่านแล้ว แต่ราคาไม่กลับมาที่ limit จึงไม่ได้ไม้",
            summary.unfilled
        ),
        "unrecorded" => "ไม่มีไม้เปิดเลย และไม่มีบันทึกว่าทำไม".into(),
        other => other.into(),
    }
}

// ------------------------------------------------------------- the fetching

/// What one fetch came back with. `Missing` is a sentence about the bot, not
/// a failure of the board: a student who has not run a backtest yet should
/// read "run this command", not "error".
enum Answer<T> {
    Have(T),
    Missing(String),
    Failed(String),
}

enum Got {
    State(Answer<BotState>),
    Audit(Answer<(Vec<Row>, usize)>),
    Back(Answer<Box<Summary>>),
}

/// Everything the board knows about the bot, and the machinery for keeping it
/// up to date.
pub struct Bot {
    pub state: Option<BotState>,
    pub state_load: Load,
    pub rows: Vec<Row>,
    pub audit_load: Load,
    pub unreadable: usize,
    pub counts: Counts,
    pub account: Account,
    pub stages: Vec<Stage>,
    pub log: Vec<Entry>,

    pub summary: Option<Summary>,
    pub back_load: Load,

    tx: Sender<Got>,
    rx: Receiver<Got>,
    // One clock and one in-flight flag per file. Sharing a flag between the
    // state and the trail would wedge both the moment one of the two answers
    // first: the pair would never be considered finished, and the board would
    // quietly stop refreshing.
    state_due: f64,
    audit_due: f64,
    back_due: f64,
    /// When each request went out, so one that never comes back is written
    /// off instead of blocking its card for the rest of the session.
    state_flight: Option<f64>,
    audit_flight: Option<f64>,
    back_flight: Option<f64>,
}

impl Default for Bot {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            state: None,
            state_load: Load::Idle,
            rows: Vec::new(),
            audit_load: Load::Idle,
            unreadable: 0,
            counts: Counts::default(),
            account: Account::default(),
            stages: stages_of(&[], None),
            log: Vec::new(),
            summary: None,
            back_load: Load::Idle,
            tx,
            rx,
            state_due: 0.0,
            audit_due: 0.0,
            back_due: 0.0,
            state_flight: None,
            audit_flight: None,
            back_flight: None,
        }
    }
}

impl Bot {
    /// Ask for anything that is due, then take whatever has arrived. Call
    /// once per frame with egui's frame time - the browser has no
    /// `Instant::now`, so time is passed in here exactly as the feed does it.
    pub fn poll(&mut self, now: f64) {
        if !crate::api::still_waiting(self.state_flight, now) && now >= self.state_due {
            self.state_flight = Some(now);
            self.state_due = now + LIVE_EVERY;
            // Only the first attempt says "loading". Saying it again every
            // two seconds would make a card that has nothing to show flicker
            // between its reason and a spinner for as long as it is open.
            if self.state_load == Load::Idle {
                self.state_load = Load::Loading;
            }
            self.fetch_state();
        }
        if !crate::api::still_waiting(self.audit_flight, now) && now >= self.audit_due {
            self.audit_flight = Some(now);
            self.audit_due = now + LIVE_EVERY;
            if self.audit_load == Load::Idle {
                self.audit_load = Load::Loading;
            }
            self.fetch_audit();
        }
        if !crate::api::still_waiting(self.back_flight, now) && now >= self.back_due {
            self.back_flight = Some(now);
            self.back_due = now + BACKTEST_EVERY;
            if self.back_load == Load::Idle {
                self.back_load = Load::Loading;
            }
            self.fetch_backtest();
        }

        let mut landed = false;
        while let Ok(got) = self.rx.try_recv() {
            match got {
                Got::State(answer) => {
                    self.state_flight = None;
                    match answer {
                        Answer::Have(state) => {
                            self.state = Some(state);
                            self.state_load = Load::Ready;
                        }
                        Answer::Missing(reason) => {
                            self.state = None;
                            self.state_load = Load::Missing(reason);
                        }
                        Answer::Failed(reason) => self.state_load = Load::Failed(reason),
                    }
                    landed = true;
                }
                Got::Audit(answer) => {
                    self.audit_flight = None;
                    match answer {
                        Answer::Have((rows, unreadable)) => {
                            self.rows = rows;
                            self.unreadable = unreadable;
                            self.audit_load = Load::Ready;
                        }
                        Answer::Missing(reason) => {
                            self.rows.clear();
                            self.audit_load = Load::Missing(reason);
                        }
                        Answer::Failed(reason) => self.audit_load = Load::Failed(reason),
                    }
                    landed = true;
                }
                Got::Back(answer) => {
                    self.back_flight = None;
                    match answer {
                        Answer::Have(summary) => {
                            self.summary = Some(*summary);
                            self.back_load = Load::Ready;
                        }
                        // The runner truncates and rewrites the file on every
                        // run, so an unreadable answer is usually a report
                        // being written right now. Keeping the last good one
                        // on screen beats blanking the card for a second.
                        Answer::Missing(reason) => {
                            if self.summary.is_none() {
                                self.back_load = Load::Missing(reason);
                            }
                        }
                        Answer::Failed(reason) => self.back_load = Load::Failed(reason),
                    }
                }
            }
        }
        if landed {
            self.rebuild();
        }
    }

    fn rebuild(&mut self) {
        self.counts = tally(&self.rows);
        self.account = account_of(&self.rows, self.state.as_ref());
        self.stages = stages_of(&self.rows, self.state.as_ref());
        self.log = log_of(&self.rows, 80);
    }

    fn fetch_state(&self) {
        let tx = self.tx.clone();
        ehttp::fetch(
            ehttp::Request::get(crate::api::url("/api/state")),
            move |result| {
                let _ = tx.send(Got::State(decode_state(result)));
            },
        );
    }

    fn fetch_audit(&self) {
        let tx = self.tx.clone();
        ehttp::fetch(
            ehttp::Request::get(crate::api::url("/api/audit")),
            move |result| {
                let _ = tx.send(Got::Audit(decode_audit(result)));
            },
        );
    }

    fn fetch_backtest(&self) {
        let tx = self.tx.clone();
        ehttp::fetch(
            ehttp::Request::get(crate::api::url("/api/backtest")),
            move |result| {
                let _ = tx.send(Got::Back(decode_backtest(result)));
            },
        );
    }

    /// True once there is something of the bot to look at. Used to tell "the
    /// bot has not run here" apart from "the bot ran and did nothing".
    pub fn has_live(&self) -> bool {
        !self.rows.is_empty() || self.state.is_some()
    }
}

/// The body of a reply, or a sentence about why there is none.
fn body(result: Result<ehttp::Response, String>) -> Result<serde_json::Value, String> {
    let response = result.map_err(|err| format!("ขอข้อมูลไม่สำเร็จ: {err}"))?;
    if !response.ok {
        return Err(format!("เซิร์ฟเวอร์ตอบ {}", response.status));
    }
    let text = response
        .text()
        .ok_or_else(|| "คำตอบไม่ใช่ข้อความ".to_string())?;
    serde_json::from_str(text).map_err(|err| format!("อ่าน JSON ไม่ได้: {err}"))
}

/// `{"present": false, "reason": ...}` is the server saying the bot has not
/// written that file. Pull the reason out, whatever the shape of the rest.
fn absent(value: &serde_json::Value) -> Option<String> {
    if value.get("present").and_then(|p| p.as_bool()) == Some(true) {
        return None;
    }
    Some(
        value
            .get("reason")
            .and_then(|r| r.as_str())
            .unwrap_or("ไม่มีข้อมูล")
            .to_owned(),
    )
}

fn decode_state(result: Result<ehttp::Response, String>) -> Answer<BotState> {
    let value = match body(result) {
        Ok(value) => value,
        Err(err) => return Answer::Failed(err),
    };
    if let Some(reason) = absent(&value) {
        return Answer::Missing(reason);
    }
    match serde_json::from_value(value.get("data").cloned().unwrap_or_default()) {
        Ok(state) => Answer::Have(state),
        Err(err) => Answer::Failed(format!("state.json รูปร่างไม่ตรง: {err}")),
    }
}

fn decode_audit(result: Result<ehttp::Response, String>) -> Answer<(Vec<Row>, usize)> {
    let value = match body(result) {
        Ok(value) => value,
        Err(err) => return Answer::Failed(err),
    };
    if let Some(reason) = absent(&value) {
        return Answer::Missing(reason);
    }
    let mut rows = Vec::new();
    if let Some(items) = value.get("rows").and_then(|r| r.as_array()) {
        for item in items {
            // One line the board cannot read must not cost the other 499.
            if let Ok(row) = serde_json::from_value::<Row>(item.clone()) {
                rows.push(row);
            }
        }
    }
    let unreadable = value
        .get("unreadable")
        .and_then(|u| u.as_u64())
        .unwrap_or(0) as usize;
    Answer::Have((rows, unreadable))
}

fn decode_backtest(result: Result<ehttp::Response, String>) -> Answer<Box<Summary>> {
    let value = match body(result) {
        Ok(value) => value,
        Err(err) => return Answer::Failed(err),
    };
    if let Some(reason) = absent(&value) {
        return Answer::Missing(reason);
    }
    let data = value.get("data").cloned().unwrap_or_default();
    let curve_total = value
        .get("curve_total")
        .and_then(|t| t.as_u64())
        .unwrap_or(0) as usize;
    let trades_total = value
        .get("trades_total")
        .and_then(|t| t.as_u64())
        .unwrap_or(0) as usize;
    Answer::Have(Box::new(summary_of(&data, curve_total, trades_total)))
}

// ------------------------------------------------------------------ printing

/// What a number that could not be measured says on screen.
///
/// Never "0.00". A profit factor of zero means every trade lost; no profit
/// factor means there was nothing to divide. A card that prints the same
/// thing for both is lying about one of them.
pub const UNMEASURED: &str = "วัดไม่ได้";

pub fn money(value: f64) -> String {
    format!("{value:.2}")
}

/// Money with its sign shown, because "-9.00" and "9.00" are the whole story.
pub fn signed(value: f64) -> String {
    format!("{value:+.2}")
}

pub fn pct(value: f64) -> String {
    format!("{value:+.2}%")
}

pub fn opt(value: Option<f64>, decimals: usize) -> String {
    match value {
        Some(value) => format!("{value:.decimals$}"),
        None => UNMEASURED.to_owned(),
    }
}

pub fn opt_pct(value: Option<f64>) -> String {
    match value {
        Some(value) => pct(value),
        None => UNMEASURED.to_owned(),
    }
}

/// Green for a gain, red for a loss, cream for flat. Never the accent
/// colour: on this board orange means "system", and this is money.
pub fn pnl_colour(value: f64) -> egui::Color32 {
    if value > 0.0 {
        theme::OK
    } else if value < 0.0 {
        theme::BAD
    } else {
        theme::MUTED
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(value: serde_json::Value) -> Row {
        serde_json::from_value(value).expect("ตัวอย่างแถวต้องอ่านได้")
    }

    fn tick(price: &str, equity: &str) -> Row {
        row(json!({
            "at": "2026-11-04T10:42:05.618914+00:00",
            "symbol": "BTC_USDT_Perp",
            "price": price,
            "equity": equity,
        }))
    }

    #[test]
    fn a_statistic_that_could_not_be_measured_never_prints_as_zero() {
        assert_eq!(opt(None, 2), UNMEASURED);
        assert_eq!(opt_pct(None), UNMEASURED);
        assert_eq!(opt(Some(0.0), 2), "0.00", "ศูนย์ที่วัดได้ ต้องพิมพ์เป็นศูนย์");
        assert_eq!(opt(Some(-1.3509), 2), "-1.35");
    }

    #[test]
    fn money_shows_its_sign_where_the_sign_is_the_point() {
        assert_eq!(signed(-9.0), "-9.00");
        assert_eq!(signed(9.0), "+9.00");
        assert_eq!(money(9.0), "9.00");
        assert_eq!(pct(-0.09), "-0.09%");
    }

    #[test]
    fn flat_pnl_is_not_coloured_as_a_win() {
        assert_eq!(pnl_colour(0.0), theme::MUTED);
        assert_eq!(pnl_colour(0.01), theme::OK);
        assert_eq!(pnl_colour(-0.01), theme::BAD);
    }

    #[test]
    fn a_number_that_was_not_measured_stays_absent_instead_of_becoming_zero() {
        assert_eq!(maybe(""), None);
        assert_eq!(maybe("  "), None);
        assert_eq!(maybe("ไม่ใช่ตัวเลข"), None);
        assert_eq!(maybe("0"), Some(0.0), "ศูนย์ที่วัดได้ ต้องไม่หาย");
        assert_eq!(maybe("-9.0000000"), Some(-9.0));
        assert_eq!(num(""), 0.0);
    }

    #[test]
    fn the_clock_is_cut_out_of_a_timestamp_without_mangling_other_text() {
        assert_eq!(
            clock("2026-11-04T10:42:05.618914+00:00"),
            "17:42:05",
            "UTC in, Thai time out"
        );
        assert_eq!(clock("ไม่มีเวลา"), "ไม่มีเวลา");
        assert_eq!(clock(""), "");
    }

    #[test]
    fn an_empty_trail_says_what_it_is_waiting_for_rather_than_nothing() {
        let stages = stages_of(&[], None);
        assert_eq!(stages.len(), 5);
        assert!(stages[0].detail.contains("audit trail"));
        assert!(stages.iter().all(|s| s.state == theme::State::Idle));
    }

    #[test]
    fn a_tick_that_only_read_the_market_lights_the_first_stage_only() {
        let rows = vec![tick("99915.96", "10000")];
        let stages = stages_of(&rows, None);
        assert_eq!(stages[0].state, theme::State::Ok);
        assert_eq!(stages[1].state, theme::State::Wip, "กลยุทธ์ยังไม่ออกสัญญาณ");
        assert_eq!(stages[2].state, theme::State::Idle);
        assert_eq!(stages[4].detail, "ไม่มีไม้เปิดอยู่");
    }

    #[test]
    fn a_price_of_zero_is_not_taken_as_the_market() {
        // A failed broker read writes a row with price 0; taking it would put
        // "0" on the card as if the market had printed it.
        let rows = vec![
            tick("99915.96", "10000"),
            row(json!({
                "at": "2026-11-04T10:43:05+00:00",
                "symbol": "BTC_USDT_Perp",
                "price": "0",
                "equity": "0",
                "error": "broker read failed: timeout",
            })),
        ];
        let account = account_of(&rows, None);
        assert_eq!(account.equity, 10_000.0, "ต้องถอยไปใช้แถวที่อ่านราคาได้");
        let stages = stages_of(&rows, None);
        assert_eq!(stages[0].state, theme::State::Bad);
        assert!(stages[0].detail.contains("broker read failed"));
    }

    #[test]
    fn an_order_that_was_only_simulated_is_never_shown_as_a_real_one() {
        let rows = vec![row(json!({
            "at": "2026-11-04T10:42:05+00:00",
            "symbol": "BTC_USDT_Perp",
            "price": "99915.96",
            "equity": "10000",
            "signal": {"side": "buy", "ref_price": "99915.96", "stop_price": "98916.80", "reason": "coin flip rolled 0.053"},
            "order": {"client_order_id": "90aa", "side": "buy", "qty": "0.091", "limit_price": "100015.8", "risk_amount": "99.97"},
            "receipt": {"broker_order_id": "paper-90aa", "simulated": true},
        }))];
        let stages = stages_of(&rows, None);
        assert_eq!(stages[3].state, theme::State::Simulated);
        assert_eq!(stages[3].metric, "paper");
        assert!(stages[3].detail.contains("จำลอง"));
    }

    #[test]
    fn a_refusal_shows_the_reason_in_words_and_keeps_the_code() {
        let rows = vec![row(json!({
            "at": "2026-11-04T10:42:05+00:00",
            "price": "99915.96",
            "equity": "10000",
            "signal": {"side": "buy", "ref_price": "1", "stop_price": "2", "reason": ""},
            "reject": {"code": "already_in_position", "detail": "ถือ buy อยู่"},
        }))];
        let stages = stages_of(&rows, None);
        assert_eq!(stages[2].state, theme::State::Bad);
        assert_eq!(stages[2].metric, "already_in_position");
        assert!(stages[2].detail.contains("มีไม้ค้างอยู่แล้ว"));
    }

    #[test]
    fn a_reject_code_the_board_has_never_heard_of_is_shown_as_itself() {
        assert_eq!(reject_label("halted"), "บอทหยุดตัวเอง");
        assert_eq!(reject_label("some_future_code"), "some_future_code");
    }

    #[test]
    fn a_halted_bot_says_so_at_the_gate_even_on_a_quiet_tick() {
        let state = BotState {
            halted: true,
            halt_reason: "3 consecutive failures".into(),
            ..BotState::default()
        };
        let stages = stages_of(&[tick("99915.96", "10000")], Some(&state));
        assert_eq!(stages[2].state, theme::State::Bad);
        assert!(stages[2].detail.contains("3 consecutive failures"));
    }

    #[test]
    fn the_account_card_reads_the_day_from_state_and_the_equity_from_the_trail() {
        let state: BotState = serde_json::from_value(json!({
            "day": "2026-11-04",
            "day_start_equity": "10000",
            "peak_equity": "10500",
            "halted": false,
            "halt_reason": "",
            "sent_order_ids": ["a", "b"],
        }))
        .unwrap();
        let account = account_of(&[tick("99915.96", "9975")], Some(&state));
        assert_eq!(account.equity, 9_975.0);
        assert_eq!(account.day_pnl, -25.0);
        assert!((account.day_pnl_pct + 0.25).abs() < 1e-9);
        assert!((account.drawdown_pct - 5.0).abs() < 1e-9);
        assert_eq!(account.sent_orders, 2);
        assert_eq!(account.day, "2026-11-04");
    }

    #[test]
    fn a_quiet_tick_is_not_an_event_worth_a_line() {
        let rows = vec![tick("1", "1"), tick("2", "1")];
        assert!(log_of(&rows, 10).is_empty(), "บอทที่ดูแล้วไม่ทำอะไร ไม่ต้องขึ้น log");
        assert_eq!(tally(&rows).ticks, 2);
    }

    #[test]
    fn closing_a_position_is_an_event_worth_a_line() {
        let rows = vec![row(json!({
            "at": "2026-11-13T12:00:00+00:00",
            "price": "68500", "equity": "10000",
            "position": {"side": "buy", "qty": "0.1", "entry_price": "68400"},
            "exit": {"reason": "price came back to the average",
                     "broker_order_id": "paper-abc", "simulated": true}
        }))];
        let log = log_of(&rows, 10);
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].kind, Kind::Exit);
        assert!(log[0].text.contains("price came back"));
        assert!(log[0].text.contains("paper"), "ต้องบอกว่าเป็นกระดาษ");
        assert_eq!(tally(&rows).exits, 1);
    }

    #[test]
    fn the_tick_that_closed_does_not_say_it_is_still_holding() {
        let rows = vec![row(json!({
            "at": "t", "price": "68500", "equity": "10000",
            "position": {"side": "buy", "qty": "0.1", "entry_price": "68400"},
            "exit": {"reason": "stop hit at 68000", "simulated": true}
        }))];
        let stages = stages_of(&rows, None);
        assert_eq!(stages[4].metric, "ปิดแล้ว");
        assert!(stages[4].detail.contains("stop hit"));
    }

    #[test]
    fn the_newest_thing_the_bot_did_is_at_the_top() {
        let rows = vec![
            row(json!({"at": "t1", "price": "1", "equity": "1",
                       "signal": {"side": "buy", "ref_price": "1", "stop_price": "1", "reason": "แรก"}})),
            row(json!({"at": "t2", "price": "1", "equity": "1", "error": "broker read failed"})),
        ];
        let log = log_of(&rows, 10);
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].kind, Kind::Error);
        assert_eq!(log[1].kind, Kind::Signal);
    }

    // ------------------------------------------------------------ backtest

    /// The real shape of `logs/backtest.json`, taken from a run of the
    /// bot. Parsed from text rather than built with `json!`, which blows the
    /// macro recursion limit on a report this wide.
    fn report() -> serde_json::Value {
        serde_json::from_str(REPORT).expect("ตัวอย่างรายงานต้องอ่านได้")
    }

    const REPORT: &str = r#"{
        "symbol": "BTC_USDT_Perp",
        "interval": "5m",
        "strategy": "RandomStrategy",
        "exchange": "grvt",
        "bars": 115198,
        "starting_equity": "10000",
        "final_equity": "9991.0000000",
        "net_pnl": "-9.0000000",
        "return_pct": "-0.0900000",
        "fees_paid": "9.0000000",
        "max_drawdown": "9.0000000",
        "max_drawdown_pct": "0.0900000",
        "cagr_pct": "-0.0821296600038310156150062300",
        "buy_and_hold_pct": "0",
        "days_underwater": "399.9861111111111111111111111",
        "worst_month": {"month": "2026-11", "return_pct": "-0.045"},
        "sharpe": "-1.350949028630028999696804431",
        "win_loss_ratio": "0",
        "profit_without_best_two": "0",
        "trades": 1,
        "wins": 0,
        "losses": 1,
        "win_rate_pct": "0",
        "profit_factor": "0",
        "expectancy_r": "-0.09086778736937755565651976374",
        "longest_losing_streak": 1,
        "exposure_pct": "99.997",
        "signals": 1,
        "unfilled_orders": 0,
        "no_trade_reason": "",
        "thin_sample_below": 30,
        "rejects": {},
        "halted_at": null,
        "halt_reason": "",
        "warnings": [],
        "equity_curve": [
            {"at": "2025-10-08T07:26:22+00:00", "equity": "10000"},
            {"at": "2026-11-12T07:11:22+00:00", "equity": "9991"}
        ],
        "trade_list": [{
            "side": "sell", "qty": "0.090",
            "entry_at": "2025-10-08T07:31:22+00:00", "entry_price": "100000",
            "exit_at": "2026-11-12T07:06:22+00:00", "exit_price": "100000",
            "net_pnl": "-9.0000000", "r_multiple": "-0.0908",
            "exit_reason": "still open at the end"
        }]
    }"#;

    #[test]
    fn the_full_decimal_strings_become_numbers_here_and_only_here() {
        let view = summary_of(&report(), 115_198, 1);
        assert_eq!(view.net_pnl, -9.0);
        assert_eq!(view.final_equity, 9_991.0);
        assert!((view.cagr_pct.unwrap() + 0.08212966).abs() < 1e-6);
        assert_eq!(view.bars, 115_198);
    }

    #[test]
    fn a_statistic_the_run_could_not_measure_arrives_as_absent() {
        let mut raw = report();
        raw["sortino"] = json!(null);
        raw["sharpe"] = json!("");
        let view = summary_of(&raw, 2, 1);
        assert_eq!(view.sortino, None, "ไม่มี sortino ต้องไม่กลายเป็น 0");
        assert_eq!(view.sharpe, None);
        // ...while a measured zero survives as a zero.
        assert_eq!(view.win_rate_pct, 0.0);
        assert_eq!(view.profit_factor, Some(0.0));
    }

    #[test]
    fn doing_nothing_wins_when_the_bot_only_matches_it() {
        let view = summary_of(&report(), 2, 1);
        // -0.09% against a flat 0%: holding wins, and a draw is a win for
        // holding too, because holding costs no fees and no attention.
        assert_eq!(view.winner, Some(Winner::Hold));

        let mut better = report();
        better["return_pct"] = json!("5");
        assert_eq!(summary_of(&better, 2, 1).winner, Some(Winner::Bot));

        let mut unknown = report();
        unknown["buy_and_hold_pct"] = json!(null);
        assert_eq!(summary_of(&unknown, 2, 1).winner, None);
    }

    #[test]
    fn a_result_that_rests_on_two_lucky_trades_is_flagged() {
        let mut raw = report();
        raw["net_pnl"] = json!("500");
        raw["profit_without_best_two"] = json!("-20");
        assert!(summary_of(&raw, 2, 1).rests_on_two);

        raw["profit_without_best_two"] = json!("300");
        assert!(!summary_of(&raw, 2, 1).rests_on_two);
    }

    #[test]
    fn a_sample_too_thin_to_mean_anything_says_so_at_the_top_of_the_notes() {
        let view = summary_of(&report(), 2, 1);
        assert!(view.thin, "1 ไม้ต้องถูกนับว่าบางเกินไป");
        assert!(view.notes[0].contains("โชค"));
        assert_eq!(view.notes.len(), NOTES.len() + 1);

        let mut many = report();
        many["trades"] = json!(80);
        let view = summary_of(&many, 2, 1);
        assert!(!view.thin);
        assert_eq!(view.notes.len(), NOTES.len());
    }

    #[test]
    fn the_threshold_for_a_thin_sample_comes_from_the_bot_not_from_here() {
        let mut raw = report();
        raw["trades"] = json!(40);
        raw["thin_sample_below"] = json!(100);
        assert!(
            summary_of(&raw, 2, 1).thin,
            "เกณฑ์ต้องมาจากไฟล์ ไม่ใช่เลขที่เขียนซ้ำในนี้"
        );
    }

    #[test]
    fn refusals_are_listed_worst_first_with_their_sentences() {
        let mut raw = report();
        raw["rejects"] = json!({"already_in_position": 7, "invalid_stop": 12});
        let view = summary_of(&raw, 2, 1);
        assert_eq!(view.rejects[0].0, "invalid_stop");
        assert_eq!(view.rejects[0].1, "stop วางผิด");
        assert_eq!(view.reject_total, 19);
    }

    #[test]
    fn a_run_with_no_trades_says_which_of_the_five_reasons_it_was() {
        let mut raw = report();
        raw["trades"] = json!(0);
        raw["no_trade_reason"] = json!("never_filled");
        raw["unfilled_orders"] = json!(3);
        assert!(summary_of(&raw, 2, 0).no_trades.contains("3 ออเดอร์"));

        raw["no_trade_reason"] = json!("all_refused");
        raw["rejects"] = json!({"invalid_stop": 4});
        let view = summary_of(&raw, 2, 0);
        assert!(view.no_trades.contains("stop วางผิด"));
        assert!(view.no_trades.contains("4 ครั้ง"));

        raw["no_trade_reason"] = json!("");
        assert_eq!(summary_of(&raw, 2, 0).no_trades, "");
    }

    #[test]
    fn the_curve_remembers_how_many_points_it_was_thinned_from() {
        let view = summary_of(&report(), 115_198, 1);
        assert_eq!(view.curve.len(), 2);
        assert_eq!(view.curve_total, 115_198);
        assert_eq!(view.curve[0].equity, 10_000.0);
    }

    #[test]
    fn trades_are_newest_first_and_the_hidden_ones_are_counted() {
        let view = summary_of(&report(), 2, 300);
        assert_eq!(view.trade_rows.len(), 1);
        assert_eq!(view.trades_hidden, 299);
        assert_eq!(view.trade_rows[0].exit, "ยังถืออยู่ตอนจบการทดสอบ");
        assert_eq!(view.trade_rows[0].net_pnl, -9.0);
    }

    #[test]
    fn a_report_with_nothing_in_it_still_produces_a_card_shaped_view() {
        let view = summary_of(&json!({}), 0, 0);
        assert_eq!(view.trades, 0);
        assert!(view.curve.is_empty());
        assert_eq!(view.winner, None);
        assert!(view.thin, "ศูนย์ไม้ก็บางเกินไป");
    }

    // ------------------------------------------------------------ fetching

    fn reply(body: &str) -> Result<ehttp::Response, String> {
        Ok(ehttp::Response {
            url: "/api/state".into(),
            ok: true,
            status: 200,
            status_text: "OK".into(),
            headers: Default::default(),
            bytes: body.as_bytes().to_vec(),
        })
    }

    #[test]
    fn a_file_the_bot_has_not_written_is_a_sentence_not_a_failure() {
        let answer = decode_state(reply(
            r#"{"present":false,"reason":"ยังไม่มี logs/state.json"}"#,
        ));
        match answer {
            Answer::Missing(reason) => assert!(reason.contains("state.json")),
            _ => panic!("ไฟล์ที่ยังไม่มี ต้องไม่ถูกนับเป็นความผิดพลาด"),
        }
    }

    #[test]
    fn a_broken_line_in_the_trail_costs_only_itself() {
        let answer = decode_audit(reply(
            r#"{"present":true,"unreadable":1,"rows":[
                {"at":"t1","price":"1","equity":"1"},
                {"at":"t2","price":"1","equity":"1","position":"ไม่ใช่ออบเจกต์"}
            ]}"#,
        ));
        match answer {
            Answer::Have((rows, unreadable)) => {
                assert_eq!(rows.len(), 1, "แถวที่อ่านได้ต้องรอด");
                assert_eq!(unreadable, 1);
            }
            _ => panic!("ควรอ่านได้"),
        }
    }

    /// A bot that will not reach for the network: every clock is set far
    /// ahead, so `poll` only drains what a test put in the channel. Without
    /// this the fetches would go out during the test, fail (there is no
    /// server), and race the answer the test is checking for.
    fn quiet() -> Bot {
        let mut bot = Bot::default();
        bot.state_due = f64::INFINITY;
        bot.audit_due = f64::INFINITY;
        bot.back_due = f64::INFINITY;
        bot
    }

    #[test]
    fn each_file_has_its_own_turn_so_one_slow_answer_cannot_stop_the_others() {
        let mut bot = quiet();
        bot.state_flight = Some(0.0);
        bot.audit_flight = Some(0.0);
        bot.back_flight = Some(0.0);
        bot.tx
            .send(Got::State(Answer::Have(BotState::default())))
            .unwrap();
        bot.poll(0.0);
        assert!(bot.state_flight.is_none(), "ไฟล์ที่ตอบแล้วต้องพร้อมถามรอบใหม่");
        assert!(
            bot.audit_flight.is_some() && bot.back_flight.is_some(),
            "ไฟล์ที่ยังไม่ตอบต้องไม่ถูกนับว่าตอบแล้ว"
        );
    }

    #[test]
    fn a_request_that_never_came_back_is_written_off_and_asked_again() {
        let mut bot = Bot::default();
        bot.audit_due = 0.0;
        bot.audit_flight = Some(0.0);
        // Still waiting, so nothing new goes out...
        bot.poll(1.0);
        assert_eq!(bot.audit_flight, Some(0.0));
        // ...until the board gives up on it, and asks again.
        bot.state_due = f64::INFINITY;
        bot.back_due = f64::INFINITY;
        bot.poll(crate::api::LOST + 1.0);
        assert_eq!(bot.audit_flight, Some(crate::api::LOST + 1.0));
    }

    #[test]
    fn a_half_written_backtest_file_does_not_wipe_the_last_good_one() {
        let mut bot = quiet();
        bot.summary = Some(summary_of(&report(), 2, 1));
        bot.back_load = Load::Ready;
        bot.tx
            .send(Got::Back(Answer::Missing("กำลังถูกเขียนใหม่".into())))
            .unwrap();
        bot.poll(100.0);
        assert!(bot.summary.is_some(), "ผลเดิมต้องยังอยู่บนจอ");
        assert_eq!(bot.back_load, Load::Ready);
    }

    #[test]
    fn a_card_with_nothing_to_show_keeps_saying_why_instead_of_flickering() {
        // The files are asked for again every couple of seconds. A card that
        // has already been told "the bot has not written this yet" must keep
        // that sentence, not blink back to "loading" on every attempt.
        let mut bot = Bot::default();
        bot.state_due = f64::INFINITY;
        bot.audit_due = f64::INFINITY;
        bot.back_due = f64::INFINITY;
        bot.tx
            .send(Got::State(Answer::Missing("ยังไม่มี logs/state.json".into())))
            .unwrap();
        bot.poll(0.0);
        let told = bot.state_load.clone();
        // ...now let the clock come round again.
        bot.state_due = 0.0;
        bot.poll(1.0);
        assert_eq!(bot.state_load, told);
    }

    #[test]
    fn with_no_backtest_at_all_the_card_is_told_why() {
        let mut bot = quiet();
        bot.tx
            .send(Got::Back(Answer::Missing(
                "ยังไม่มี logs/backtest.json".into(),
            )))
            .unwrap();
        bot.poll(100.0);
        assert!(bot.summary.is_none());
        assert_eq!(
            bot.back_load,
            Load::Missing("ยังไม่มี logs/backtest.json".into())
        );
    }

    #[test]
    fn what_arrives_is_derived_once_rather_than_every_frame() {
        let mut bot = quiet();
        bot.tx
            .send(Got::Audit(Answer::Have((
                vec![tick("99915.96", "10000")],
                0,
            ))))
            .unwrap();
        bot.tx
            .send(Got::State(Answer::Have(BotState::default())))
            .unwrap();
        bot.poll(100.0);
        assert_eq!(bot.counts.ticks, 1);
        assert_eq!(bot.stages[0].state, theme::State::Ok);
        assert_eq!(bot.account.equity, 10_000.0);
        assert!(bot.has_live());
    }
}
