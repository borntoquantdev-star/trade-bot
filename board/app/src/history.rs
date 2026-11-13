//! Past runs, and what each day came to.
//!
//! Two lists the board fetches on a slow clock, because neither changes
//! while you watch: a run ends once, and a day ends once.
//!
//! Both arrive already summarised by the board's own server, which reads the
//! **whole** of `runs.jsonl` and `audit.jsonl` rather than the tail the live
//! cards are handed. A history that quietly stopped at the 500th-last tick
//! would be worse than no history.

use std::sync::mpsc::{channel, Receiver, Sender};

use serde::Deserialize;

use crate::candles::Load;

/// How often the two lists are asked for again, in seconds.
const EVERY: f64 = 10.0;

/// One run of the bot, from the line it wrote when it started to the line it
/// wrote when it stopped.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Run {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub started_at: String,
    #[serde(default)]
    pub ended_at: String,
    #[serde(default)]
    pub strategy: String,
    #[serde(default)]
    pub exchange: String,
    #[serde(default)]
    pub symbol: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub ticks: u64,
    #[serde(default)]
    pub orders: u64,
    #[serde(default)]
    pub exits: u64,
    #[serde(default)]
    pub rejects: u64,
    /// What the account did over the run. `None` when it cannot be measured,
    /// never a zero standing in for "unknown".
    #[serde(default)]
    pub change: Option<f64>,
    /// False for a run that was killed before it could write its last line,
    /// and for one that is still going.
    #[serde(default)]
    pub finished: bool,
    #[serde(default)]
    pub halted: bool,
    #[serde(default)]
    pub reason: String,
}

impl Run {
    /// `23:47` in Thai time out of an ISO timestamp, or an empty string.
    pub fn clock(at: &str) -> String {
        crate::time::hhmm(at)
    }

    /// The Thai date the run started on.
    pub fn day(&self) -> String {
        crate::time::date(&self.started_at)
    }

    /// How long it ran, in whole seconds, when both ends are known.
    ///
    /// Subtracted as clock readings rather than parsed into dates: the two
    /// strings come from the same process on the same machine minutes
    /// apart, and the only case this gets wrong is a run that crossed
    /// midnight - which it says, by refusing to answer.
    pub fn seconds(&self) -> Option<u64> {
        let start = clock_seconds(&self.started_at)?;
        let end = clock_seconds(&self.ended_at)?;
        if end < start {
            return None;
        }
        Some(end - start)
    }

    /// What to call the state it ended in.
    pub fn state(&self) -> theme::State {
        if self.halted {
            theme::State::Bad
        } else if !self.finished {
            theme::State::Wip
        } else if self.orders > 0 {
            theme::State::Ok
        } else {
            theme::State::Idle
        }
    }
}

use crate::theme;

fn clock_seconds(at: &str) -> Option<u64> {
    let time = at.get(11..19)?;
    let mut parts = time.split(':');
    let hours: u64 = parts.next()?.parse().ok()?;
    let minutes: u64 = parts.next()?.parse().ok()?;
    let seconds: u64 = parts.next()?.parse().ok()?;
    Some(hours * 3600 + minutes * 60 + seconds)
}

/// One calendar day of the bot's life.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Day {
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub ticks: u64,
    #[serde(default)]
    pub orders: u64,
    #[serde(default)]
    pub exits: u64,
    #[serde(default)]
    pub rejects: u64,
    #[serde(default)]
    pub errors: u64,
    #[serde(default)]
    pub first_equity: String,
    #[serde(default)]
    pub last_equity: String,
    #[serde(default)]
    pub change: Option<f64>,
}

impl Day {
    /// `(year, month, day)` out of `2026-11-13`.
    pub fn parts(&self) -> Option<(i32, u32, u32)> {
        let year = self.date.get(..4)?.parse().ok()?;
        let month = self.date.get(5..7)?.parse().ok()?;
        let day = self.date.get(8..10)?.parse().ok()?;
        Some((year, month, day))
    }
}

/// Which weekday a date falls on, 0 = Monday.
///
/// Zeller's congruence, because the alternative is a date library in a
/// WebAssembly bundle for one answer a calendar needs.
pub fn weekday(year: i32, month: u32, day: u32) -> usize {
    let (y, m) = if month < 3 {
        (year - 1, month + 12)
    } else {
        (year, month)
    };
    let k = y % 100;
    let j = y / 100;
    let h = (day as i32 + (13 * (m as i32 + 1)) / 5 + k + k / 4 + j / 4 + 5 * j) % 7;
    // Zeller counts Saturday as 0; a week here starts on Monday.
    ((h + 5) % 7) as usize
}

/// How many days are in a month.
pub fn days_in(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => 30,
    }
}

pub const MONTHS: [&str; 12] = [
    "มกราคม",
    "กุมภาพันธ์",
    "มีนาคม",
    "เมษายน",
    "พฤษภาคม",
    "มิถุนายน",
    "กรกฎาคม",
    "สิงหาคม",
    "กันยายน",
    "ตุลาคม",
    "พฤศจิกายน",
    "ธันวาคม",
];

pub const WEEKDAYS: [&str; 7] = ["จ", "อ", "พ", "พฤ", "ศ", "ส", "อา"];

// ------------------------------------------------------------- the fetching

enum Got {
    Runs(Result<Vec<Run>, String>),
    Days(Result<Vec<Day>, String>),
}

pub struct Past {
    pub runs: Vec<Run>,
    pub runs_load: Load,
    pub days: Vec<Day>,
    pub days_load: Load,
    tx: Sender<Got>,
    rx: Receiver<Got>,
    runs_due: f64,
    days_due: f64,
    runs_flight: Option<f64>,
    days_flight: Option<f64>,
}

impl Default for Past {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            runs: Vec::new(),
            runs_load: Load::Idle,
            days: Vec::new(),
            days_load: Load::Idle,
            tx,
            rx,
            runs_due: 0.0,
            days_due: 0.0,
            runs_flight: None,
            days_flight: None,
        }
    }
}

impl Past {
    pub fn poll(&mut self, now: f64) {
        if !crate::api::still_waiting(self.runs_flight, now) && now >= self.runs_due {
            self.runs_flight = Some(now);
            self.runs_due = now + EVERY;
            if self.runs_load == Load::Idle {
                self.runs_load = Load::Loading;
            }
            let tx = self.tx.clone();
            ehttp::fetch(
                ehttp::Request::get(crate::api::url("/api/runs")),
                move |result| {
                    let _ = tx.send(Got::Runs(read(result, "runs")));
                },
            );
        }
        if !crate::api::still_waiting(self.days_flight, now) && now >= self.days_due {
            self.days_flight = Some(now);
            self.days_due = now + EVERY;
            if self.days_load == Load::Idle {
                self.days_load = Load::Loading;
            }
            let tx = self.tx.clone();
            ehttp::fetch(
                ehttp::Request::get(crate::api::url("/api/days")),
                move |result| {
                    let _ = tx.send(Got::Days(read(result, "days")));
                },
            );
        }
        while let Ok(got) = self.rx.try_recv() {
            match got {
                Got::Runs(answer) => {
                    self.runs_flight = None;
                    match answer {
                        Ok(runs) => {
                            self.runs = runs;
                            self.runs_load = Load::Ready;
                        }
                        Err(reason) => self.runs_load = Load::Missing(reason),
                    }
                }
                Got::Days(answer) => {
                    self.days_flight = None;
                    match answer {
                        Ok(days) => {
                            self.days = days;
                            self.days_load = Load::Ready;
                        }
                        Err(reason) => self.days_load = Load::Missing(reason),
                    }
                }
            }
        }
    }

    /// Everything in one day, for the card that answers "today".
    pub fn day(&self, date: &str) -> Option<&Day> {
        self.days.iter().find(|day| day.date == date)
    }

    /// The last day the bot wrote anything on, which is what a calendar
    /// opens to.
    pub fn newest(&self) -> Option<&Day> {
        self.days.last()
    }
}

/// The list inside a reply, or a sentence about why there is none.
fn read<T: serde::de::DeserializeOwned>(
    result: Result<ehttp::Response, String>,
    key: &str,
) -> Result<Vec<T>, String> {
    let response = result.map_err(|err| format!("ขอข้อมูลไม่สำเร็จ: {err}"))?;
    if !response.ok {
        return Err(format!("เซิร์ฟเวอร์ตอบ {}", response.status));
    }
    let text = response
        .text()
        .ok_or_else(|| "คำตอบไม่ใช่ข้อความ".to_string())?;
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|err| format!("อ่าน JSON ไม่ได้: {err}"))?;
    if value.get("present").and_then(|p| p.as_bool()) != Some(true) {
        return Err(value
            .get("reason")
            .and_then(|r| r.as_str())
            .unwrap_or("ยังไม่มีข้อมูล")
            .to_owned());
    }
    serde_json::from_value(value.get(key).cloned().unwrap_or_default())
        .map_err(|err| format!("รูปร่างข้อมูลไม่ตรง: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_knows_how_long_it_took() {
        let run = Run {
            started_at: "2026-11-13T16:47:00+00:00".into(),
            ended_at: "2026-11-13T16:48:01+00:00".into(),
            ..Default::default()
        };
        assert_eq!(run.seconds(), Some(61));
        assert_eq!(Run::clock(&run.started_at), "23:47", "เวลาไทย");
    }

    /// Clock arithmetic cannot see a date change, so it says so instead of
    /// answering with a negative hour.
    #[test]
    fn a_run_that_crossed_midnight_refuses_to_guess_its_length() {
        let run = Run {
            started_at: "2026-11-13T23:59:00+00:00".into(),
            ended_at: "2026-11-14T00:01:00+00:00".into(),
            ..Default::default()
        };
        assert_eq!(run.seconds(), None);
    }

    #[test]
    fn a_run_still_going_is_not_drawn_as_one_that_finished() {
        let open = Run {
            finished: false,
            ..Default::default()
        };
        assert_eq!(open.state(), theme::State::Wip);
        let halted = Run {
            finished: true,
            halted: true,
            ..Default::default()
        };
        assert_eq!(halted.state(), theme::State::Bad);
    }

    #[test]
    fn the_calendar_knows_which_square_a_date_goes_in() {
        // 13 November 2026 is a Friday.
        assert_eq!(weekday(2026, 11, 13), 4);
        assert_eq!(WEEKDAYS[weekday(2026, 11, 13)], "ศ");
        // 1 January 2026 is a Thursday.
        assert_eq!(weekday(2026, 1, 1), 3);
        // 29 February 2024 was a Thursday - the leap year path.
        assert_eq!(weekday(2024, 2, 29), 3);
    }

    #[test]
    fn february_knows_about_leap_years() {
        assert_eq!(days_in(2024, 2), 29);
        assert_eq!(days_in(2026, 2), 28);
        assert_eq!(days_in(2000, 2), 29, "ปี 2000 เป็นปีอธิกสุรทิน");
        assert_eq!(days_in(1900, 2), 28, "ปี 1900 ไม่ใช่");
        assert_eq!(days_in(2026, 11), 30);
    }

    #[test]
    fn a_date_comes_apart_into_numbers() {
        let day = Day {
            date: "2026-11-13".into(),
            ..Default::default()
        };
        assert_eq!(day.parts(), Some((2026, 11, 13)));
        let broken = Day {
            date: "nonsense".into(),
            ..Default::default()
        };
        assert_eq!(broken.parts(), None);
    }
}
