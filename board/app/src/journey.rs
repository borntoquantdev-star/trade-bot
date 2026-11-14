//! One order's trip to the exchange, followed to the end.
//!
//! **ออกแบบโดย Claude (คืน 14 พ.ย. 2026) — รอรีวิว**
//!
//! The pipeline's fourth card used to stop at "sent". But "sent" is where a
//! trader's questions start: did it arrive, how long did the exchange take
//! to answer, is it sitting in the book waiting for somebody to trade with
//! it or did it fill the instant it landed, all of it or half, at what
//! price? This module answers them by lining up two records:
//!
//! * **the bot's row** - when it decided, when the order left, how long the
//!   answer took, and the id the exchange files it under;
//! * **the exchange's lists** - the order as GRVT holds it (status, how much
//!   traded) and every fill against it, each stamped to the nanosecond.
//!
//! Nothing here sends anything. It reads both records and lines them up.

use crate::account::{Fill, Order};
use crate::bot::Row;
use crate::time;

/// How long a sent order may stay missing from the exchange's lists before
/// the board stops saying "looking" and says "not found". The lists are
/// polled every couple of seconds while a trip is live, so this is many
/// chances, not one.
pub const LOOK_FOR: f64 = 90.0;

/// How far either side of the send an exchange order may be stamped and
/// still be taken for this one, when there is no id to match on.
const GUESS_BEFORE: f64 = 3.0;
const GUESS_AFTER: f64 = 30.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum What {
    /// A limit order opening a position.
    Entry,
    /// A reduce-only market order closing one.
    Exit,
}

/// The bot's half of the story, read from one audit row.
#[derive(Debug, Clone, PartialEq)]
pub struct Trip {
    pub what: What,
    /// The row's own timestamp - also how the replay finds the tick.
    pub tick_at: String,
    /// Unix seconds: when the tick decided, and when the order left.
    pub decided: Option<f64>,
    pub sent: Option<f64>,
    /// The exchange's answer to the send, there and back.
    pub answered_ms: Option<f64>,
    pub is_buy: bool,
    pub qty: String,
    /// Empty for an exit, which goes at market.
    pub limit: String,
    pub venue_id: String,
    pub simulated: bool,
    /// What the exchange said when it would not take the order.
    pub refused: Option<String>,
    /// Why the bot sent it: the signal's reason, or the exit's.
    pub why: String,
}

impl Trip {
    /// The trip on this row, if the row sent anything. The engine closes on
    /// one tick and opens on another, so a row holds at most one.
    pub fn from_row(row: &Row) -> Option<Trip> {
        let decided = time::unix_of(&row.at);
        if let Some(exit) = &row.exit {
            let position = row.position.clone().unwrap_or_default();
            return Some(Trip {
                what: What::Exit,
                tick_at: row.at.clone(),
                decided,
                sent: time::unix_of(&exit.sent_at),
                answered_ms: exit.answered_ms,
                // Closing a long sells; closing a short buys.
                is_buy: position.side != "buy",
                qty: position.qty,
                limit: String::new(),
                venue_id: exit.venue_id.clone(),
                simulated: exit.simulated,
                refused: row.error.clone(),
                why: exit.reason.clone(),
            });
        }
        let order = row.order.as_ref()?;
        let receipt = row.receipt.as_ref();
        Some(Trip {
            what: What::Entry,
            tick_at: row.at.clone(),
            decided,
            sent: time::unix_of(&order.sent_at),
            answered_ms: order.answered_ms,
            is_buy: order.side == "buy",
            qty: order.qty.clone(),
            limit: order.limit_price.clone(),
            venue_id: receipt.map(|r| r.venue_id.clone()).unwrap_or_default(),
            simulated: receipt.is_some_and(|r| r.simulated),
            refused: if receipt.is_none() {
                row.error.clone()
            } else {
                None
            },
            why: row
                .signal
                .as_ref()
                .map(|signal| signal.reason.clone())
                .unwrap_or_default(),
        })
    }

    /// The moment to measure from: when it left, or failing that (a row from
    /// before the bot timed its sends), when it was decided.
    pub fn start(&self) -> Option<f64> {
        self.sent.or(self.decided)
    }

    pub fn side(&self) -> &'static str {
        if self.is_buy {
            "ซื้อ"
        } else {
            "ขาย"
        }
    }

    pub fn kind(&self) -> &'static str {
        match self.what {
            What::Entry => "เปิดไม้ · limit",
            What::Exit => "ปิดไม้ · market",
        }
    }
}

/// Every trip in the trail, oldest first.
pub fn trips(rows: &[Row]) -> Vec<Trip> {
    rows.iter().filter_map(Trip::from_row).collect()
}

/// The exchange's half of the story.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Seen {
    pub order: Option<Order>,
    /// Oldest first.
    pub fills: Vec<Fill>,
    /// Matched on time and side, not on id: an old row, or an exit whose id
    /// the reply did not echo. Said on screen, because it could be wrong.
    pub guessed: bool,
}

impl Seen {
    pub fn filled_qty(&self) -> f64 {
        if let Some(filled) = self.order.as_ref().and_then(|o| o.filled.as_ref()) {
            return filled.value;
        }
        self.fills
            .iter()
            .filter_map(|fill| fill.size.as_ref().map(|size| size.value))
            .sum()
    }

    pub fn size(&self, trip: &Trip) -> Option<f64> {
        self.order
            .as_ref()
            .and_then(|o| o.size.as_ref().map(|size| size.value))
            .or_else(|| trip.qty.parse().ok())
    }

    /// Size-weighted price of the fills, when there are any.
    pub fn average(&self) -> Option<f64> {
        let mut size = 0.0;
        let mut value = 0.0;
        for fill in &self.fills {
            let (Some(s), Some(p)) = (&fill.size, &fill.price) else {
                continue;
            };
            size += s.value;
            value += s.value * p.value;
        }
        if size > 0.0 {
            return Some(value / size);
        }
        self.order
            .as_ref()
            .and_then(|o| o.avg_price.as_ref())
            .map(|avg| avg.value)
            .filter(|avg| *avg > 0.0)
    }
}

/// Find the trip's order and fills in what the exchange last said.
///
/// `open` and `history` can both hold the order - it moves from one to the
/// other when it finishes, and the two are polled at different times - so
/// the one whose state moved last wins.
pub fn find(trip: &Trip, open: &[Order], history: &[Order], fills: &[Fill]) -> Seen {
    let mut seen = Seen::default();
    if !trip.venue_id.is_empty() {
        seen.order = open
            .iter()
            .chain(history)
            .filter(|order| order.client_order_id == trip.venue_id)
            .max_by(|a, b| {
                a.updated
                    .unwrap_or(0.0)
                    .total_cmp(&b.updated.unwrap_or(0.0))
            })
            .cloned();
    } else if let Some(start) = trip.start() {
        let exit = trip.what == What::Exit;
        seen.order = open
            .iter()
            .chain(history)
            .filter(|order| order.is_buy == trip.is_buy && order.reduce_only == exit)
            .filter_map(|order| {
                let created = order.created?;
                let gap = created - start;
                (-GUESS_BEFORE..=GUESS_AFTER)
                    .contains(&gap)
                    .then_some((gap.abs(), order))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, order)| order.clone());
        seen.guessed = seen.order.is_some();
    }

    let client = seen
        .order
        .as_ref()
        .map(|order| order.client_order_id.clone())
        .unwrap_or_else(|| trip.venue_id.clone());
    let order_id = seen
        .order
        .as_ref()
        .map(|order| order.order_id.clone())
        .unwrap_or_default();
    seen.fills = fills
        .iter()
        .filter(|fill| {
            (!client.is_empty() && fill.client_order_id == client)
                || (!order_id.is_empty() && order_id != "0x00" && fill.order_id == order_id)
        })
        .cloned()
        .collect();
    seen.fills
        .sort_by(|a, b| a.when.unwrap_or(0.0).total_cmp(&b.when.unwrap_or(0.0)));
    seen
}

/// Where the trip has got to.
#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    /// The exchange answered the send with a refusal.
    Refused(String),
    /// Paper trading: there is no book, the fill is assumed.
    Simulated,
    /// Sent and answered, but the board has no key to read the account
    /// with, so this is as far as it can follow.
    Blind,
    /// Sent; not in the exchange's lists yet.
    Looking,
    /// In the book. `filled` is how much of it has traded, 0..1.
    Waiting {
        filled: f64,
    },
    Filled,
    Cancelled {
        filled: f64,
        reason: String,
    },
    Rejected(String),
    /// Too old to still be in the lists the board can read.
    Lost,
}

impl Phase {
    /// Still moving: worth asking the exchange again soon, and worth a
    /// clock that counts up on screen.
    pub fn is_live(&self) -> bool {
        matches!(self, Phase::Looking | Phase::Waiting { .. })
    }

    pub fn is_bad(&self) -> bool {
        matches!(
            self,
            Phase::Refused(_) | Phase::Rejected(_) | Phase::Cancelled { .. } | Phase::Lost
        )
    }

    /// One line, for the top of the card and for the stage card.
    pub fn headline(&self) -> String {
        match self {
            Phase::Refused(_) => "ตลาดไม่รับคำสั่ง".into(),
            Phase::Simulated => "จำลอง · ถือว่าเต็มทันที".into(),
            Phase::Blind => "ตลาดตอบรับแล้ว · ไม่มีกุญแจอ่านบัญชีให้ตามต่อ".into(),
            Phase::Looking => "ส่งแล้ว · กำลังหาในสมุดของตลาด".into(),
            Phase::Waiting { filled } if *filled <= 0.0 => "รอคิวในสมุด · ยังไม่มีใครจับคู่".into(),
            Phase::Waiting { filled } => {
                format!("จับคู่ไปแล้ว {:.0}% · ที่เหลือรอคิว", filled * 100.0)
            }
            Phase::Filled => "จับคู่ครบทั้งก้อน".into(),
            Phase::Cancelled { filled, .. } if *filled > 0.0 => {
                format!("ยกเลิก · จับคู่ได้ {:.0}% ก่อนยกเลิก", filled * 100.0)
            }
            Phase::Cancelled { .. } => "ยกเลิก · ไม่ได้จับคู่เลย".into(),
            Phase::Rejected(_) => "ตลาดปฏิเสธ".into(),
            Phase::Lost => "ไม่เจอในประวัติล่าสุดของตลาด".into(),
        }
    }
}

fn fraction(seen: &Seen, trip: &Trip) -> f64 {
    match seen.size(trip) {
        Some(size) if size > 0.0 => (seen.filled_qty() / size).clamp(0.0, 1.0),
        _ => 0.0,
    }
}

/// GRVT's reasons arrive as enum names; the common ones in Thai, with the
/// code kept so it can still be looked up.
pub fn reason(code: &str) -> String {
    let thai = match code {
        "" | "UNSPECIFIED" => return String::new(),
        "CLIENT_CANCEL" => "บอทหรือคนยกเลิกเอง",
        "CLIENT_BULK_CANCEL" => "ยกเลิกทั้งหมดพร้อมกัน",
        "EXPIRED" => "หมดอายุ",
        "FAIL_POST_ONLY" => "post-only แต่จะกลายเป็น taker",
        "FAIL_REDUCE_ONLY" => "reduce-only แต่ไม่มีไม้ให้ลด",
        "IOC_REMAINDER" | "IOC_FOK_FAIL" => "ส่วนที่จับไม่ได้ทันทีถูกทิ้ง",
        "INSUFFICIENT_MARGIN" | "MARGIN_CHECK_FAILED" => "มาร์จินไม่พอ",
        "SELF_TRADE" | "SELF_MATCHED_SUBACCOUNT" => "จะจับคู่กับคำสั่งของตัวเอง",
        "MARKET_UNAVAILABLE" => "ตลาดปิดหรือหยุดชั่วคราว",
        other => return other.to_owned(),
    };
    format!("{thai} ({code})")
}

pub fn phase(trip: &Trip, seen: &Seen, has_key: bool, now: f64) -> Phase {
    if let Some(why) = &trip.refused {
        return Phase::Refused(why.clone());
    }
    if trip.simulated {
        return Phase::Simulated;
    }
    if let Some(order) = &seen.order {
        let filled = fraction(seen, trip);
        return match order.status.as_str() {
            "FILLED" => Phase::Filled,
            "CANCELLED" => Phase::Cancelled {
                filled,
                reason: reason(&order.reject),
            },
            "REJECTED" => Phase::Rejected(reason(&order.reject)),
            // OPEN, PENDING, or something new: still working.
            _ => Phase::Waiting { filled },
        };
    }
    // No order in the lists, but every unit already traded: the order has
    // aged out of the history page while its fills have not.
    if !seen.fills.is_empty() && fraction(seen, trip) >= 0.999 {
        return Phase::Filled;
    }
    if !has_key {
        return Phase::Blind;
    }
    match trip.start() {
        Some(start) if now - start < LOOK_FOR => Phase::Looking,
        _ => Phase::Lost,
    }
}

/// The handful of durations a person asks about, in seconds.
///
/// Two clocks are involved and they do not agree: this computer's stamps the
/// decision and the send, the exchange's stamps the book and the fills. On
/// the machine this was built on the exchange ran about a second ahead,
/// which made every order look as if it took a second to reach a book that
/// had in fact already answered in a fifth of that. So each duration is
/// measured within one clock, and the exchange's stamps are moved onto this
/// computer's clock by `skew` before the two are compared.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Times {
    /// Tick decided -> order left. The bot's own thinking and checking.
    pub to_send: Option<f64>,
    /// There and back to the exchange.
    pub answer: Option<f64>,
    /// Order left -> exchange took it into the book. When the two clocks
    /// disagree this is an estimate: half the round trip.
    pub to_book: Option<f64>,
    /// In the book -> first fill; or -> now, while it is still waiting.
    pub waited: Option<f64>,
    /// Order left -> last fill (or final state); or -> now while live.
    pub total: Option<f64>,
    /// How far the exchange's clock runs ahead of this computer's, in
    /// seconds; 0 when nothing shows they differ.
    pub skew: f64,
}

/// The exchange must have taken the order in after it was sent and before
/// its answer came back. A stamp outside that window is not a slow exchange,
/// it is a clock that disagrees - and by about this much.
pub fn skew_of(trip: &Trip, seen: &Seen) -> f64 {
    let booked = seen.order.as_ref().and_then(|order| order.created);
    match (trip.sent, booked, trip.answered_ms) {
        (Some(sent), Some(booked), Some(ms)) => {
            let answer = ms / 1000.0;
            let gap = booked - sent;
            if (0.0..=answer).contains(&gap) {
                0.0
            } else {
                booked - (sent + answer / 2.0)
            }
        }
        _ => 0.0,
    }
}

pub fn times(trip: &Trip, seen: &Seen, phase: &Phase, now: f64) -> Times {
    let skew = skew_of(trip, seen);
    // Every exchange stamp, on this computer's clock.
    let ours = |at: Option<f64>| at.map(|at| at - skew);
    let start = trip.start();
    let booked = ours(seen.order.as_ref().and_then(|order| order.created));
    let first = ours(seen.fills.iter().find_map(|fill| fill.when));
    let last = ours(seen.fills.iter().rev().find_map(|fill| fill.when));
    let updated = ours(seen.order.as_ref().and_then(|o| o.updated));
    let finished = match phase {
        Phase::Filled => last.or(updated),
        Phase::Cancelled { .. } | Phase::Rejected(_) => updated,
        _ if phase.is_live() => Some(now),
        _ => None,
    };
    let waited = match (booked.or(start), first) {
        (Some(from), Some(to)) => Some(to - from),
        (Some(from), None) if phase.is_live() => Some(now - from),
        _ => None,
    };
    Times {
        to_send: match (trip.decided, trip.sent) {
            (Some(decided), Some(sent)) => Some(sent - decided),
            _ => None,
        },
        answer: trip.answered_ms.map(|ms| ms / 1000.0),
        to_book: match (trip.sent, booked) {
            (Some(sent), Some(booked)) => Some((booked - sent).max(0.0)),
            _ => None,
        },
        waited: waited.map(|w| w.max(0.0)),
        total: match (start, finished) {
            (Some(from), Some(to)) => Some((to - from).max(0.0)),
            _ => None,
        },
        skew,
    }
}

/// A duration the way a person says it: milliseconds under a second,
/// tenths under a minute, minutes and seconds after.
pub fn span(seconds: f64) -> String {
    let seconds = seconds.max(0.0);
    if seconds < 1.0 {
        format!("{:.0} ms", seconds * 1000.0)
    } else if seconds < 60.0 {
        format!("{seconds:.1} วิ")
    } else if seconds < 3600.0 {
        let whole = seconds as u64;
        format!("{} นาที {} วิ", whole / 60, whole % 60)
    } else {
        let whole = seconds as u64;
        format!("{} ชม. {} นาที", whole / 3600, whole % 3600 / 60)
    }
}

/// How a step on the timeline is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Done,
    /// Happening now: drawn pulsing.
    Now,
    Todo,
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub name: &'static str,
    pub detail: String,
    pub mark: Mark,
}

/// The five steps of a trip, each with what is known about it.
pub fn steps(trip: &Trip, seen: &Seen, phase: &Phase, now: f64) -> Vec<Step> {
    let t = times(trip, seen, phase, now);
    let mut out = Vec::with_capacity(5);

    // 1. The bot decided.
    out.push(Step {
        name: "บอทตัดสินใจ",
        detail: match trip.decided {
            Some(at) if trip.why.is_empty() => time::clock_tenths(at),
            Some(at) => format!("{} · {}", time::clock_tenths(at), trip.why),
            None => trip.tick_at.clone(),
        },
        mark: Mark::Done,
    });

    // 2. It left, and the exchange answered.
    let answered = match (t.to_send, t.answer) {
        (Some(gap), Some(answer)) => {
            format!("ออกไป {} หลังตัดสินใจ · ตลาดตอบใน {}", span(gap), span(answer))
        }
        (None, Some(answer)) => format!("ตลาดตอบใน {}", span(answer)),
        _ => "บอทรุ่นที่รันตอนนั้นยังไม่ได้จับเวลาส่ง".into(),
    };
    out.push(match phase {
        Phase::Refused(why) => Step {
            name: "ส่งไปตลาด",
            detail: format!("{answered} · ตลาดตอบว่าไม่รับ: {why}"),
            mark: Mark::Failed,
        },
        _ => Step {
            name: "ส่งไปตลาด",
            detail: answered,
            mark: Mark::Done,
        },
    });

    // 3. The exchange took it into its book.
    let booked = seen.order.as_ref().and_then(|order| order.created);
    out.push(Step {
        name: "เข้าสมุดคำสั่ง",
        detail: match (phase, booked) {
            (Phase::Refused(_), _) => "ไม่ถึงสมุด".into(),
            (Phase::Simulated, _) => "เทรดกระดาษไม่มีสมุดจริง".into(),
            (Phase::Blind, _) => "ต้องมีกุญแจอ่านบัญชีที่เซิร์ฟเวอร์ถึงจะเห็น".into(),
            (Phase::Looking, _) => match trip.start() {
                Some(start) => format!("กำลังหา · {} แล้ว", span(now - start)),
                None => "กำลังหา".into(),
            },
            (Phase::Lost, _) => "ไม่เจอในรายการล่าสุดของตลาด (เก่าเกินไป)".into(),
            (_, Some(at)) => {
                let after = match t.to_book {
                    Some(gap) if t.skew.abs() >= 0.05 => format!(
                        " · ราว {} หลังส่ง (นาฬิกาเครื่องนี้{}กว่าตลาด {} หักออกแล้ว)",
                        span(gap),
                        if t.skew > 0.0 {
                            "ช้า"
                        } else {
                            "เร็ว"
                        },
                        span(t.skew.abs())
                    ),
                    Some(gap) => format!(" · {} หลังส่ง", span(gap)),
                    None => String::new(),
                };
                let id = if seen.guessed {
                    " · จับคู่จากเวลา (ไม่มีเลขอ้างอิง)"
                } else {
                    ""
                };
                format!("ตลาดรับเข้าคิว {}{after}{id}", time::clock_tenths(at))
            }
            _ => "ตลาดรับแล้ว".into(),
        },
        mark: match phase {
            Phase::Refused(_) | Phase::Lost => Mark::Failed,
            Phase::Blind => Mark::Todo,
            Phase::Looking => Mark::Now,
            _ => Mark::Done,
        },
    });

    // 4. Waiting for somebody to trade with it.
    let size = seen.size(trip).unwrap_or(0.0);
    let traded = seen.filled_qty();
    let pieces = seen.fills.len();
    let taker = !seen.fills.is_empty() && seen.fills.iter().all(|fill| fill.is_taker);
    out.push(Step {
        name: "รอจับคู่",
        detail: match phase {
            Phase::Waiting { .. } => format!(
                "รอมาแล้ว {} · จับได้ {} / {}",
                span(t.waited.unwrap_or(0.0)),
                trim(traded),
                trim(size)
            ),
            Phase::Filled if taker => {
                format!("ไม่ต้องรอ: ราคาที่ตั้งชนคำสั่งที่รออยู่ทันที (taker) · {pieces} ไม้")
            }
            Phase::Filled => match t.waited {
                Some(waited) => {
                    format!("รอ {} มีคนมาจับคู่ (maker) · {pieces} ไม้", span(waited))
                }
                None => format!("{pieces} ไม้"),
            },
            Phase::Simulated => "ข้าม: กระดาษถือว่าจับคู่ทันที".into(),
            Phase::Cancelled { .. } => {
                format!("จับได้ {} / {} ก่อนถูกยกเลิก", trim(traded), trim(size))
            }
            _ => String::new(),
        },
        mark: match phase {
            Phase::Waiting { .. } => Mark::Now,
            Phase::Filled | Phase::Simulated => Mark::Done,
            Phase::Cancelled { .. } => Mark::Failed,
            _ => Mark::Todo,
        },
    });

    // 5. How it ended.
    let average = seen
        .average()
        .map(|avg| format!(" @ เฉลี่ย {}", trim(avg)))
        .unwrap_or_default();
    let total = t
        .total
        .map(|total| format!(" · รวม {} ตั้งแต่ส่ง", span(total)))
        .unwrap_or_default();
    out.push(Step {
        name: "จบ",
        detail: match phase {
            Phase::Filled => format!("เต็ม {}{average}{total}", trim(traded.max(size))),
            Phase::Cancelled { reason, .. } => format!("ยกเลิก {reason}{total}"),
            Phase::Rejected(reason) => format!("ตลาดปฏิเสธ {reason}"),
            Phase::Refused(_) => "จบตั้งแต่ตอนส่ง".into(),
            Phase::Simulated => "เต็ม (สมมติ)".into(),
            _ => String::new(),
        },
        mark: match phase {
            Phase::Filled | Phase::Simulated => Mark::Done,
            p if p.is_bad() => Mark::Failed,
            _ => Mark::Todo,
        },
    });
    out
}

/// A quantity or price without the zeros a float leaves behind.
pub fn trim(value: f64) -> String {
    let text = format!("{value:.6}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text.is_empty() || text == "-0" {
        "0".into()
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot;
    use crate::exchange::Num;

    fn num(text: &str) -> Option<Num> {
        Num::from(Some(&serde_json::Value::String(text.into())))
    }

    const AT: &str = "2026-11-13T19:20:02.700000+00:00";
    const SENT: &str = "2026-11-13T19:20:02.750000+00:00";

    fn entry_row() -> Row {
        Row {
            at: AT.into(),
            signal: Some(bot::Signal {
                side: "sell".into(),
                reason: "price is +0.033% from its average".into(),
                ..Default::default()
            }),
            order: Some(bot::Order {
                client_order_id: "bc8c395b46ca3dc5cbe790baef58dcd3".into(),
                side: "sell".into(),
                qty: "0.143".into(),
                limit_price: "68896.6".into(),
                sent_at: SENT.into(),
                answered_ms: Some(380.0),
                ..Default::default()
            }),
            receipt: Some(bot::Receipt {
                broker_order_id: "0x00".into(),
                venue_id: "3163306331".into(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn sent() -> f64 {
        time::unix_of(SENT).unwrap()
    }

    fn exchange_order(status: &str, filled: &str) -> Order {
        Order {
            is_buy: false,
            size: num("0.143"),
            price: num("68896.6"),
            filled: num(filled),
            status: status.into(),
            client_order_id: "3163306331".into(),
            order_id: "0xabc".into(),
            created: Some(sent() + 0.2),
            updated: Some(sent() + 1.5),
            ..Default::default()
        }
    }

    fn fill(size: &str, after: f64, taker: bool) -> Fill {
        Fill {
            size: num(size),
            price: num("68900"),
            is_taker: taker,
            client_order_id: "3163306331".into(),
            order_id: "0xabc".into(),
            when: Some(sent() + after),
            ..Default::default()
        }
    }

    #[test]
    fn a_row_that_sent_an_order_becomes_a_trip_with_its_timings() {
        let trip = Trip::from_row(&entry_row()).unwrap();
        assert_eq!(trip.what, What::Entry);
        assert!(!trip.is_buy);
        assert_eq!(trip.venue_id, "3163306331");
        assert_eq!(trip.answered_ms, Some(380.0));
        assert!((trip.sent.unwrap() - trip.decided.unwrap() - 0.05).abs() < 1e-6);
    }

    #[test]
    fn a_quiet_row_is_not_a_trip() {
        assert!(Trip::from_row(&Row::default()).is_none());
    }

    #[test]
    fn an_exit_sells_a_long_and_goes_at_market() {
        let row = Row {
            at: AT.into(),
            position: Some(bot::Position {
                side: "buy".into(),
                qty: "0.145".into(),
                ..Default::default()
            }),
            exit: Some(bot::Exit {
                reason: "price came back to the average".into(),
                venue_id: "4087321".into(),
                sent_at: SENT.into(),
                answered_ms: Some(120.0),
                ..Default::default()
            }),
            ..Default::default()
        };
        let trip = Trip::from_row(&row).unwrap();
        assert_eq!(trip.what, What::Exit);
        assert!(!trip.is_buy, "ปิด long ต้องขาย");
        assert_eq!(trip.qty, "0.145");
        assert!(trip.limit.is_empty());
    }

    #[test]
    fn the_order_is_found_by_the_id_the_exchange_files_it_under() {
        let trip = Trip::from_row(&entry_row()).unwrap();
        let mut other = exchange_order("FILLED", "0.143");
        other.client_order_id = "1".into();
        let seen = find(&trip, &[], &[other, exchange_order("OPEN", "0")], &[]);
        assert_eq!(seen.order.unwrap().status, "OPEN");
        assert!(!seen.guessed);
    }

    #[test]
    fn when_the_order_is_in_both_lists_the_fresher_one_wins() {
        let trip = Trip::from_row(&entry_row()).unwrap();
        let mut open = exchange_order("OPEN", "0");
        open.updated = Some(sent() + 0.3);
        let done = exchange_order("FILLED", "0.143");
        let seen = find(&trip, &[open], &[done], &[]);
        assert_eq!(seen.order.unwrap().status, "FILLED");
    }

    #[test]
    fn without_an_id_the_order_is_guessed_from_time_and_side_and_says_so() {
        let mut trip = Trip::from_row(&entry_row()).unwrap();
        trip.venue_id.clear();
        let mut far = exchange_order("FILLED", "0.143");
        far.created = Some(sent() + 600.0);
        let seen = find(&trip, &[], &[far, exchange_order("FILLED", "0.143")], &[]);
        assert!(seen.guessed);
        assert_eq!(seen.order.unwrap().created, Some(sent() + 0.2));

        let mut buy = exchange_order("FILLED", "0.143");
        buy.is_buy = true;
        assert!(
            find(&trip, &[], &[buy], &[]).order.is_none(),
            "ฝั่งผิดต้องไม่จับคู่"
        );
    }

    #[test]
    fn a_resting_order_is_waiting_and_counts_up() {
        let trip = Trip::from_row(&entry_row()).unwrap();
        let seen = find(&trip, &[exchange_order("OPEN", "0")], &[], &[]);
        let now = sent() + 12.2;
        let phase = phase(&trip, &seen, true, now);
        assert_eq!(phase, Phase::Waiting { filled: 0.0 });
        assert!(phase.is_live());
        let t = times(&trip, &seen, &phase, now);
        assert!((t.waited.unwrap() - 12.0).abs() < 1e-6, "นับจากตอนเข้าสมุด");
        let wait = &steps(&trip, &seen, &phase, now)[3];
        assert_eq!(wait.mark, Mark::Now);
        assert!(wait.detail.contains("12.0 วิ"), "{}", wait.detail);
    }

    #[test]
    fn a_half_filled_order_says_how_much() {
        let trip = Trip::from_row(&entry_row()).unwrap();
        let seen = find(
            &trip,
            &[exchange_order("OPEN", "0.0715")],
            &[],
            &[fill("0.0715", 2.0, false)],
        );
        let phase = phase(&trip, &seen, true, sent() + 5.0);
        assert_eq!(phase.headline(), "จับคู่ไปแล้ว 50% · ที่เหลือรอคิว");
    }

    #[test]
    fn a_filled_order_reports_its_whole_trip() {
        let trip = Trip::from_row(&entry_row()).unwrap();
        let seen = find(
            &trip,
            &[],
            &[exchange_order("FILLED", "0.143")],
            &[fill("0.1", 0.25, true), fill("0.043", 0.3, true)],
        );
        let phase = phase(&trip, &seen, true, sent() + 100.0);
        assert_eq!(phase, Phase::Filled);
        let t = times(&trip, &seen, &phase, sent() + 100.0);
        assert!(
            (t.total.unwrap() - 0.3).abs() < 1e-6,
            "ถึงไม้สุดท้าย ไม่ใช่ถึงตอนนี้"
        );
        assert!((t.answer.unwrap() - 0.38).abs() < 1e-9);
        let all = steps(&trip, &seen, &phase, sent() + 100.0);
        assert!(all.iter().all(|step| step.mark == Mark::Done), "{all:?}");
        assert!(all[3].detail.contains("taker"), "{}", all[3].detail);
        assert!(all[4].detail.contains("300 ms"), "{}", all[4].detail);
        assert!(
            all[1].detail.contains("50 ms") && all[1].detail.contains("380 ms"),
            "{}",
            all[1].detail
        );
    }

    #[test]
    fn a_cancelled_order_names_the_reason() {
        let trip = Trip::from_row(&entry_row()).unwrap();
        let mut order = exchange_order("CANCELLED", "0");
        order.reject = "EXPIRED".into();
        let seen = find(&trip, &[], &[order], &[]);
        let phase = phase(&trip, &seen, true, sent() + 100.0);
        assert!(phase.is_bad());
        let end = &steps(&trip, &seen, &phase, sent() + 100.0)[4];
        assert_eq!(end.mark, Mark::Failed);
        assert!(end.detail.contains("หมดอายุ"), "{}", end.detail);
    }

    #[test]
    fn a_refused_send_stops_at_the_second_step() {
        let mut row = entry_row();
        row.receipt = None;
        row.error = Some("order rejected by broker: insufficient margin".into());
        let trip = Trip::from_row(&row).unwrap();
        let phase = phase(&trip, &Seen::default(), true, sent() + 1.0);
        assert!(matches!(phase, Phase::Refused(_)));
        let all = steps(&trip, &Seen::default(), &phase, sent() + 1.0);
        assert_eq!(all[1].mark, Mark::Failed);
        assert!(all[1].detail.contains("insufficient margin"));
    }

    #[test]
    fn a_fresh_order_not_yet_listed_is_being_looked_for_then_given_up_on() {
        let trip = Trip::from_row(&entry_row()).unwrap();
        let none = Seen::default();
        assert_eq!(phase(&trip, &none, true, sent() + 2.0), Phase::Looking);
        assert_eq!(
            phase(&trip, &none, true, sent() + LOOK_FOR + 1.0),
            Phase::Lost
        );
        assert_eq!(phase(&trip, &none, false, sent() + 2.0), Phase::Blind);
    }

    #[test]
    fn paper_trading_says_it_is_pretend() {
        let mut row = entry_row();
        row.receipt.as_mut().unwrap().simulated = true;
        let trip = Trip::from_row(&row).unwrap();
        let phase = phase(&trip, &Seen::default(), false, sent());
        assert_eq!(phase, Phase::Simulated);
        assert!(!phase.is_live());
    }

    #[test]
    fn an_old_row_without_timings_still_makes_a_trip() {
        let mut row = entry_row();
        let order = row.order.as_mut().unwrap();
        order.sent_at.clear();
        order.answered_ms = None;
        let trip = Trip::from_row(&row).unwrap();
        assert_eq!(trip.start(), trip.decided);
        let all = steps(&trip, &Seen::default(), &Phase::Lost, sent());
        assert!(all[1].detail.contains("ยังไม่ได้จับเวลา"));
    }

    #[test]
    fn a_clock_that_disagrees_with_the_exchange_is_taken_out_of_the_timings() {
        // Measured for real on 14 Nov: answered in 185 ms, yet stamped by the
        // exchange a full second after the send. The second is the clocks.
        let mut trip = Trip::from_row(&entry_row()).unwrap();
        trip.answered_ms = Some(200.0);
        let mut order = exchange_order("FILLED", "0.143");
        order.created = Some(sent() + 1.0);
        let seen = find(&trip, &[], &[order], &[fill("0.143", 1.003, true)]);
        let phase = phase(&trip, &seen, true, sent() + 50.0);
        let t = times(&trip, &seen, &phase, sent() + 50.0);
        assert!((t.skew - 0.9).abs() < 1e-6, "{t:?}");
        assert!((t.to_book.unwrap() - 0.1).abs() < 1e-6, "ครึ่งหนึ่งของไปกลับ");
        assert!((t.total.unwrap() - 0.103).abs() < 1e-6, "{t:?}");
        let book = &steps(&trip, &seen, &phase, sent() + 50.0)[2];
        assert!(book.detail.contains("ช้ากว่าตลาด 900 ms"), "{}", book.detail);
    }

    #[test]
    fn a_stamp_inside_the_round_trip_needs_no_correction() {
        let trip = Trip::from_row(&entry_row()).unwrap();
        let seen = find(&trip, &[], &[exchange_order("FILLED", "0.143")], &[]);
        assert_eq!(skew_of(&trip, &seen), 0.0);
    }

    #[test]
    fn durations_read_the_way_people_say_them() {
        assert_eq!(span(0.38), "380 ms");
        assert_eq!(span(12.25), "12.2 วิ");
        assert_eq!(span(125.0), "2 นาที 5 วิ");
        assert_eq!(span(-1.0), "0 ms");
    }

    #[test]
    fn exchange_reasons_are_translated_and_keep_their_code() {
        assert_eq!(reason("EXPIRED"), "หมดอายุ (EXPIRED)");
        assert_eq!(reason("SOMETHING_NEW"), "SOMETHING_NEW");
        assert_eq!(reason("UNSPECIFIED"), "");
    }
}
