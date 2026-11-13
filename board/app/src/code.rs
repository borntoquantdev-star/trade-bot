//! The bot's own source, fetched so a card can show the function that ran.
//!
//! The five stage cards used to print only what came out of a stage. That
//! answers "what did it decide" and never answers "is it running at all",
//! which is the question somebody watching a quiet bot actually has. So each
//! card now carries the lines of real Python that really ran on the last
//! tick, and the wide card beside them shows the whole function.
//!
//! Nothing here guesses. The audit row names the file, the function and every
//! line the interpreter stepped through (see `tradebot/trace.py`); this module
//! only decides which of those functions belongs on which card, and asks the
//! board's server for the text.
//!
//! Fetches are keyed by the function, not by the line: a trace names a
//! function by the line its `def` sits on, which does not move while a tick
//! wanders around inside it. So a card that is watching a loop asks once,
//! not sixty times a second.

use std::collections::BTreeMap;
use std::sync::mpsc::{channel, Receiver, Sender};

use crate::bot::Call;
use crate::candles::Load;

/// How long a function is trusted before it is asked for again.
///
/// Not forever: somebody learning edits a strategy and wants the card to
/// follow. Not every frame either, for the obvious reason.
const STALE_AFTER: f64 = 30.0;

/// How often the five-function plan is asked for again. Rarely: it only
/// changes when somebody edits the config file.
const PLAN_EVERY: f64 = 60.0;

/// The two ways a Python docstring opens.
const DOUBLE_DOC: &str = "\"\"\"";
const SINGLE_DOC: &str = "'''";

/// A file and the line its `def` is on - what a trace calls a function.
pub type Key = (String, usize);

pub fn key(call: &Call) -> Key {
    (call.file.clone(), call.line)
}

/// One function, as it reads on disk right now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Source {
    pub file: String,
    pub func: String,
    /// The file line `lines[0]` came from, 1-based.
    pub first: usize,
    pub lines: Vec<String>,
}

impl Source {
    /// The last file line this piece holds, 1-based and inclusive.
    pub fn last(&self) -> usize {
        self.first + self.lines.len().saturating_sub(1)
    }

    /// The text of one file line, by its number in the file.
    pub fn line(&self, number: usize) -> Option<&str> {
        if number < self.first {
            return None;
        }
        self.lines.get(number - self.first).map(String::as_str)
    }

    /// Every line in this function, as `(number, text, times it ran)`.
    ///
    /// `hits` is what the trace recorded, so a line that did not run comes
    /// back with a zero rather than being left out - a card showing only the
    /// lines that ran would be a card with no shape to it.
    pub fn walk<'a>(
        &'a self,
        hits: &'a BTreeMap<usize, u64>,
    ) -> impl Iterator<Item = (usize, &'a str, u64)> + 'a {
        self.lines.iter().enumerate().map(move |(offset, text)| {
            let number = self.first + offset;
            (
                number,
                text.as_str(),
                hits.get(&number).copied().unwrap_or(0),
            )
        })
    }

    /// The first line of this function that ran, if any did.
    pub fn entered(&self, hits: &BTreeMap<usize, u64>) -> Option<usize> {
        hits.keys()
            .copied()
            .find(|number| *number >= self.first && *number <= self.last())
    }

    /// The first lines of this function that ran, for a card too small to
    /// hold all of it.
    ///
    /// The `def` line is left out on purpose: the card header already says
    /// which function this is, and a signature is the one line that says
    /// nothing about which way the code went. A one-line function gets it
    /// back, because then there is nothing else to show.
    pub fn ran<'a>(
        &'a self,
        hits: &'a BTreeMap<usize, u64>,
        max: usize,
    ) -> Vec<(usize, &'a str, u64)> {
        let body: Vec<(usize, &str, u64)> = self
            .walk(hits)
            .filter(|(number, _, count)| *count > 0 && *number > self.first)
            .take(max)
            .collect();
        if !body.is_empty() {
            return body;
        }
        self.walk(hits)
            .filter(|(_, _, count)| *count > 0)
            .take(max)
            .collect()
    }

    /// Leading spaces every non-blank line of this function shares.
    ///
    /// A method sits four spaces into its class and its body eight; none of
    /// that says anything about the code, and a narrow card would rather
    /// spend the room on the code.
    pub fn indent(&self) -> usize {
        self.lines
            .iter()
            .filter(|line| !line.trim().is_empty())
            .map(|line| line.len() - line.trim_start().len())
            .min()
            .unwrap_or(0)
    }

    /// The first lines of the body, for a card with no trace to show.
    ///
    /// What a stage card falls back to before the bot has ever run: the code
    /// is still the code, it simply has nothing lit up on it yet.
    pub fn head(&self, max: usize) -> Vec<(usize, &str, u64)> {
        // The signature may run over several lines, and a docstring often
        // follows it: neither is the code. Five lines of a card spent on
        // "Send an approved order as a limit..." are five lines not showing
        // the send.
        self.lines
            .iter()
            .enumerate()
            .skip(self.body_start())
            .take(max)
            .map(|(offset, text)| (self.first + offset, text.as_str(), 0))
            .collect()
    }

    /// Where the code of the body starts, past the signature and any
    /// docstring, as an offset into `lines`.
    fn body_start(&self) -> usize {
        // Past the signature: the first line that closes its brackets and
        // ends with a colon.
        let mut open = 0i32;
        let mut index = 0;
        while index < self.lines.len() {
            let line = &self.lines[index];
            for character in line.chars() {
                match character {
                    '(' | '[' | '{' => open += 1,
                    ')' | ']' | '}' => open -= 1,
                    _ => {}
                }
            }
            index += 1;
            if open <= 0 && line.trim_end().ends_with(':') {
                break;
            }
        }
        // Past a docstring, one line or many, in either kind of quote.
        let Some(first) = self.lines.get(index) else {
            return index.min(self.lines.len());
        };
        let trimmed = first.trim();
        let Some(quote) = [DOUBLE_DOC, SINGLE_DOC]
            .into_iter()
            .find(|quote| trimmed.starts_with(quote))
        else {
            return index;
        };
        if trimmed.len() >= 2 * quote.len() && trimmed[quote.len()..].contains(quote) {
            return index + 1;
        }
        (index + 1..self.lines.len())
            .find(|end| self.lines[*end].contains(quote))
            .map(|end| end + 1)
            .unwrap_or(index)
    }

    /// The steps of `walk` that are inside this function, in order.
    ///
    /// A tick wanders out of a function and back again; the steps in
    /// between belong to somebody else's card.
    pub fn inside(&self, walk: &[usize]) -> Vec<usize> {
        let (first, last) = (self.first, self.last());
        walk.iter()
            .copied()
            .filter(|number| *number >= first && *number <= last)
            .collect()
    }

    /// `max` lines of this function with `line` in the middle of them.
    ///
    /// What a narrow card shows while the walk is running: a window that
    /// travels rather than a fixed five lines, so the card keeps up with
    /// the line that is executing.
    pub fn around<'a>(
        &'a self,
        line: usize,
        max: usize,
        hits: &'a BTreeMap<usize, u64>,
    ) -> Vec<(usize, &'a str, u64)> {
        if max == 0 || self.lines.is_empty() {
            return Vec::new();
        }
        let last = self.last();
        let half = max / 2;
        // Clamped at both ends, so the window is always `max` lines when the
        // function is long enough to fill it.
        let start = line
            .saturating_sub(half)
            .min(last.saturating_sub(max.saturating_sub(1)))
            .max(self.first);
        self.walk(hits)
            .filter(move |(number, _, _)| *number >= start)
            .take(max)
            .collect()
    }

    /// How many of this function's lines ran, and how many there are.
    pub fn covered(&self, hits: &BTreeMap<usize, u64>) -> (usize, usize) {
        let ran = self.walk(hits).filter(|(_, _, count)| *count > 0).count();
        (ran, self.lines.len())
    }
}

// ----------------------------------------------------- which card shows what

/// A rule for picking the function a stage card shows: the file's path must
/// contain `file`, and the function must be named `func`. An empty string
/// matches anything, so `("strategies/", "")` means "whatever ran in the
/// strategy".
type Rule = (&'static str, &'static str);

/// The functions each stage would like to show, most wanted first.
///
/// These name the code that *does* the stage's job, which is not always the
/// engine: reading the market is the broker's `fetch_*`, and sending the
/// order is the broker's `place_order`. Falling back to `tick` is honest -
/// that is where the engine does the work itself.
fn rules(stage: usize) -> &'static [Rule] {
    match stage {
        // 1. the market and the account, as the broker reads them
        0 => &[
            ("", "fetch_mark_price"),
            ("", "fetch_equity"),
            ("", "fetch_positions"),
            ("core/engine.py", "tick"),
        ],
        // 2. the strategy, whichever one is configured
        1 => &[
            ("strategies/", "get_signal"),
            ("strategies/", "get_exit"),
            ("strategies/", ""),
        ],
        // 3. the gate - one function, and the most important one on the board
        2 => &[("risk.py", "risk_gate"), ("risk.py", "")],
        // 4. the broker, actually sending something. Only the two
        //    functions that send: "any broker function" let a quiet tick's
        //    price read stand in for an order, which is the one thing this
        //    card must never suggest happened.
        3 => &[("", "place_order"), ("", "close_position")],
        // 5. the position: closing it is `_close`, holding it is whatever
        //    read it back off the exchange
        4 => &[
            ("core/engine.py", "_close"),
            ("", "close_position"),
            ("", "fetch_positions"),
        ],
        _ => &[],
    }
}

/// Which functions this stage could show, best first.
///
/// Every candidate really ran: the list comes from the trace, so a stage with
/// nothing to show is a stage whose code did not run this tick, and the card
/// says so rather than drawing an old picture.
pub fn candidates(stage: usize, calls: &[Call]) -> Vec<Call> {
    let mut out: Vec<Call> = Vec::new();
    for (file, func) in rules(stage) {
        for call in calls {
            let matches = (file.is_empty() || call.file.contains(file))
                && (func.is_empty() || call.func == *func);
            if matches && !out.iter().any(|kept| kept == call) {
                out.push(call.clone());
            }
        }
    }
    out
}

// ------------------------------------------------------------- the fetching

enum Answer {
    Have(Source),
    Missing(String),
    Failed(String),
}

/// What came back from the two routes this module asks.
enum Got {
    One(Key, Answer),
    /// The five functions this bot runs, and whether the bot itself said so
    /// (rather than a config file on disk being read over its shoulder).
    Plan(Vec<Call>, bool),
}

struct Held {
    load: Load,
    source: Option<Source>,
    /// When this one may be asked for again.
    due: f64,
    /// When a request went out, so one that never comes back is written off
    /// rather than blocking the card for the rest of the session.
    flight: Option<f64>,
}

/// Every function the board has asked the server for.
pub struct Code {
    held: BTreeMap<Key, Held>,
    /// The five functions worked out from the bot's config file, in stage
    /// order. What the cards show until a tick leaves a trace behind.
    plan: Vec<Call>,
    /// True when the plan came from the running bot rather than a config
    /// file. A card showing a file it only guessed at has to say so.
    plan_from_bot: bool,
    plan_due: f64,
    plan_flight: Option<f64>,
    tx: Sender<Got>,
    rx: Receiver<Got>,
}

impl Default for Code {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            held: BTreeMap::new(),
            plan: Vec::new(),
            plan_from_bot: false,
            plan_due: 0.0,
            plan_flight: None,
            tx,
            rx,
        }
    }
}

impl Code {
    /// Take in whatever arrived since the last frame, and ask for the plan
    /// if it is time to.
    pub fn poll(&mut self, now: f64) {
        if !crate::api::still_waiting(self.plan_flight, now) && now >= self.plan_due {
            self.plan_flight = Some(now);
            self.plan_due = now + PLAN_EVERY;
            fetch_plan(self.tx.clone());
        }
        while let Ok(got) = self.rx.try_recv() {
            let (key, answer) = match got {
                Got::One(key, answer) => (key, answer),
                Got::Plan(plan, from_bot) => {
                    self.plan_flight = None;
                    // An empty answer is a server that cannot find the config;
                    // keeping the last good plan beats blanking five cards.
                    if !plan.is_empty() {
                        self.plan = plan;
                        self.plan_from_bot = from_bot;
                    }
                    continue;
                }
            };
            let Some(held) = self.held.get_mut(&key) else {
                continue;
            };
            held.flight = None;
            match answer {
                Answer::Have(source) => {
                    held.source = Some(source);
                    held.load = Load::Ready;
                }
                // A function the server cannot find is usually a bot running
                // from a different folder than the board was pointed at, so
                // the card keeps whatever it already had and says why.
                Answer::Missing(reason) => held.load = Load::Missing(reason),
                Answer::Failed(reason) => held.load = Load::Failed(reason),
            }
        }
    }

    /// Make sure this function is on its way, or already here.
    pub fn want(&mut self, key: &Key, now: f64) {
        let held = self.held.entry(key.clone()).or_insert(Held {
            load: Load::Idle,
            source: None,
            due: 0.0,
            flight: None,
        });
        if crate::api::still_waiting(held.flight, now) || now < held.due {
            return;
        }
        held.flight = Some(now);
        held.due = now + STALE_AFTER;
        if held.load == Load::Idle {
            held.load = Load::Loading;
        }
        fetch(self.tx.clone(), key.clone());
    }

    pub fn load(&self, key: &Key) -> Load {
        self.held
            .get(key)
            .map(|held| held.load.clone())
            .unwrap_or(Load::Idle)
    }

    /// Hand the board a function without going near a network.
    ///
    /// Tests only. What is being tested is what a card does with the source,
    /// which has nothing to do with how it arrived.
    #[cfg(test)]
    pub fn hold(&mut self, key: Key, source: Source) {
        self.held.insert(
            key,
            Held {
                load: Load::Ready,
                source: Some(source),
                due: f64::INFINITY,
                flight: None,
            },
        );
    }

    pub fn source(&self, key: &Key) -> Option<&Source> {
        self.held.get(key).and_then(|held| held.source.as_ref())
    }

    /// Hand the board the five-function plan without a network.
    ///
    /// Tests only, and the twin of `hold`: what a card does with a plan has
    /// nothing to do with how the plan arrived.
    #[cfg(test)]
    pub fn hold_plan(&mut self, plan: Vec<Call>) {
        self.plan = plan;
        self.plan_due = f64::INFINITY;
    }

    #[cfg(test)]
    pub fn hold_plan_from_bot(&mut self, plan: Vec<Call>) {
        self.hold_plan(plan);
        self.plan_from_bot = true;
    }

    /// Did the running bot name these functions itself?
    pub fn from_bot(&self) -> bool {
        self.plan_from_bot
    }

    /// The function this stage would run, according to the config.
    ///
    /// Indexed by stage, so an entry the server could not work out stays in
    /// the list as a blank rather than shifting every stage after it onto
    /// the wrong file - which would be a board that lies quietly.
    pub fn planned(&self, stage: usize) -> Option<&Call> {
        self.plan.get(stage).filter(|call| !call.file.is_empty())
    }
}

fn fetch_plan(tx: Sender<Got>) {
    ehttp::fetch(
        ehttp::Request::get(crate::api::url("/api/stages")),
        move |result| {
            let (plan, from_bot) = read_plan(result);
            let _ = tx.send(Got::Plan(plan, from_bot));
        },
    );
}

/// The five functions out of `/api/stages`, or an empty list.
///
/// Empty covers every failure on purpose: a board that cannot reach its own
/// server has bigger problems than this card, and the cards it feeds all
/// cope with having nothing.
fn read_plan(result: Result<ehttp::Response, String>) -> (Vec<Call>, bool) {
    let nothing = (Vec::new(), false);
    let Ok(response) = result else {
        return nothing;
    };
    if !response.ok {
        return nothing;
    }
    let Some(text) = response.text() else {
        return nothing;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return nothing;
    };
    let from_bot = value
        .get("from_bot")
        .and_then(|told| told.as_bool())
        .unwrap_or(false);
    (read_stages(&value), from_bot)
}

/// Pure half of the above, so the shape can be tested without a socket.
fn read_stages(value: &serde_json::Value) -> Vec<Call> {
    value
        .get("stages")
        .and_then(|stages| stages.as_array())
        .map(|stages| {
            stages
                .iter()
                .map(|stage| Call {
                    file: stage
                        .get("file")
                        .and_then(|f| f.as_str())
                        .unwrap_or("")
                        .to_owned(),
                    func: stage
                        .get("func")
                        .and_then(|f| f.as_str())
                        .unwrap_or("")
                        .to_owned(),
                    line: stage.get("line").and_then(|l| l.as_u64()).unwrap_or(1) as usize,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn fetch(tx: Sender<Got>, key: Key) {
    let path = format!("/api/code?file={}&line={}", encode(&key.0), key.1);
    let sent = key.clone();
    ehttp::fetch(ehttp::Request::get(crate::api::url(&path)), move |result| {
        let _ = tx.send(Got::One(sent, decode(result)));
    });
}

/// Enough percent-encoding for a path like `tradebot/core/engine.py`.
///
/// The names that travel here come from the trace, which writes them from a
/// package directory: letters, digits, dots, dashes, underscores and slashes.
/// Anything else is escaped rather than trusted.
pub fn encode(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'-' | b'_' | b'/' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn decode(result: Result<ehttp::Response, String>) -> Answer {
    let response = match result {
        Ok(response) => response,
        Err(err) => return Answer::Failed(format!("ขอโค้ดไม่สำเร็จ: {err}")),
    };
    if !response.ok {
        return Answer::Failed(format!("เซิร์ฟเวอร์ตอบ {}", response.status));
    }
    let Some(text) = response.text() else {
        return Answer::Failed("คำตอบไม่ใช่ข้อความ".to_string());
    };
    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(err) => return Answer::Failed(format!("อ่าน JSON ไม่ได้: {err}")),
    };
    read(&value)
}

/// The reply, turned into a function or into a sentence about why there is
/// none. Separate from the fetch so the shape can be tested without a socket.
fn read(value: &serde_json::Value) -> Answer {
    if value.get("present").and_then(|p| p.as_bool()) != Some(true) {
        return Answer::Missing(
            value
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("ไม่มีโค้ด")
                .to_owned(),
        );
    }
    let lines: Vec<String> = value
        .get("lines")
        .and_then(|l| l.as_array())
        .map(|array| {
            array
                .iter()
                .map(|line| line.as_str().unwrap_or("").to_owned())
                .collect()
        })
        .unwrap_or_default();
    if lines.is_empty() {
        return Answer::Missing("ไฟล์ว่าง".to_string());
    }
    Answer::Have(Source {
        file: value
            .get("file")
            .and_then(|f| f.as_str())
            .unwrap_or("")
            .to_owned(),
        func: value
            .get("func")
            .and_then(|f| f.as_str())
            .unwrap_or("")
            .to_owned(),
        // A `first` of zero would make every line number one out; treat a
        // missing one as "this is the top of the file", which it is.
        first: value
            .get("first")
            .and_then(|f| f.as_u64())
            .filter(|f| *f > 0)
            .unwrap_or(1) as usize,
        lines,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(file: &str, func: &str, line: usize) -> Call {
        Call {
            file: file.to_string(),
            func: func.to_string(),
            line,
        }
    }

    fn a_tick() -> Vec<Call> {
        vec![
            call("tradebot/core/engine.py", "tick", 208),
            call("tradebot/brokers/grvt.py", "fetch_mark_price", 300),
            call("tradebot/brokers/grvt.py", "fetch_equity", 320),
            call("tradebot/strategies/ping_pong.py", "get_signal", 77),
            call("tradebot/risk.py", "risk_gate", 92),
            call("tradebot/brokers/grvt.py", "place_order", 360),
        ]
    }

    #[test]
    fn each_stage_picks_the_function_that_does_its_job() {
        let calls = a_tick();
        assert_eq!(candidates(0, &calls)[0].func, "fetch_mark_price");
        assert_eq!(candidates(1, &calls)[0].func, "get_signal");
        assert_eq!(candidates(2, &calls)[0].func, "risk_gate");
        assert_eq!(candidates(3, &calls)[0].func, "place_order");
    }

    #[test]
    fn the_strategy_card_shows_the_strategy_that_is_configured() {
        // Not a name baked in here: whichever strategy file ran is the one
        // the card shows, which is the whole point of reading it off a trace.
        let calls = vec![call("tradebot/strategies/ma_cross.py", "get_signal", 40)];
        let picked = candidates(1, &calls);
        assert_eq!(picked[0].file, "tradebot/strategies/ma_cross.py");
    }

    #[test]
    fn a_stage_whose_code_did_not_run_has_nothing_to_show() {
        // A tick that read the market and stopped: no signal, so no gate and
        // no order. The cards for those stages must stay empty rather than
        // draw the last picture they had.
        let calls = vec![
            call("tradebot/core/engine.py", "tick", 208),
            call("tradebot/strategies/ping_pong.py", "get_signal", 77),
        ];
        assert!(candidates(2, &calls).is_empty(), "the gate never ran");
        assert!(
            candidates(3, &calls).is_empty(),
            "nothing was sent, so the send-order card has nothing that ran to show"
        );
    }

    #[test]
    fn a_stage_lists_every_function_it_could_show_in_order() {
        let picked = candidates(0, &a_tick());
        let names: Vec<&str> = picked.iter().map(|call| call.func.as_str()).collect();
        assert_eq!(
            names,
            vec!["fetch_mark_price", "fetch_equity", "tick"],
            "best first, and no duplicates"
        );
    }

    #[test]
    fn a_function_is_asked_for_once_however_far_the_tick_wanders_inside_it() {
        let a = key(&call("tradebot/risk.py", "risk_gate", 92));
        let b = key(&call("tradebot/risk.py", "risk_gate", 92));
        assert_eq!(
            a, b,
            "the def line names the function, not the line running"
        );
    }

    // ----------------------------------------------------------- the source

    fn source() -> Source {
        Source {
            file: "tradebot/risk.py".into(),
            func: "risk_gate".into(),
            first: 92,
            lines: vec![
                "def risk_gate(signal, state):".into(),
                "    if state.halted:".into(),
                "        return Reject(HALTED)".into(),
                "    return size(signal)".into(),
            ],
        }
    }

    #[test]
    fn a_line_is_found_by_its_number_in_the_file_not_its_place_in_the_piece() {
        let source = source();
        assert_eq!(source.line(92), Some("def risk_gate(signal, state):"));
        assert_eq!(source.line(95), Some("    return size(signal)"));
        assert_eq!(source.last(), 95);
        assert_eq!(source.line(91), None, "before this piece");
        assert_eq!(source.line(96), None, "after it");
    }

    #[test]
    fn walking_a_function_keeps_the_lines_that_did_not_run() {
        let hits: BTreeMap<usize, u64> = [(92, 1), (93, 1), (95, 1)].into_iter().collect();
        let walked: Vec<(usize, u64)> = source()
            .walk(&hits)
            .map(|(number, _, count)| (number, count))
            .collect();
        assert_eq!(walked, vec![(92, 1), (93, 1), (94, 0), (95, 1)]);
    }

    #[test]
    fn how_much_of_a_function_ran_is_counted_against_the_whole_function() {
        let hits: BTreeMap<usize, u64> = [(92, 1), (93, 1), (95, 1)].into_iter().collect();
        assert_eq!(source().covered(&hits), (3, 4));
        assert_eq!(source().entered(&hits), Some(92));
    }

    #[test]
    fn the_strip_leaves_out_the_signature_and_shows_where_the_code_went() {
        let hits: BTreeMap<usize, u64> = [(92, 1), (93, 1), (95, 1)].into_iter().collect();
        let strip: Vec<usize> = source()
            .ran(&hits, 4)
            .into_iter()
            .map(|(number, _, _)| number)
            .collect();
        assert_eq!(strip, vec![93, 95], "the def line is not news");
    }

    #[test]
    fn a_strip_with_room_for_two_shows_the_first_two() {
        let hits: BTreeMap<usize, u64> = [(92, 1), (93, 1), (94, 1), (95, 1)].into_iter().collect();
        assert_eq!(source().ran(&hits, 2).len(), 2);
    }

    #[test]
    fn a_function_whose_only_line_that_ran_is_its_def_still_shows_it() {
        let hits: BTreeMap<usize, u64> = [(92, 1)].into_iter().collect();
        let source = source();
        let strip = source.ran(&hits, 4);
        assert_eq!(strip.len(), 1);
        assert_eq!(strip[0].0, 92);
    }

    #[test]
    fn lines_from_another_function_do_not_count_as_this_one_running() {
        // The trace holds every line of the file, not only this function's.
        let hits: BTreeMap<usize, u64> = [(10, 5), (300, 2)].into_iter().collect();
        assert_eq!(source().covered(&hits), (0, 4));
        assert_eq!(source().entered(&hits), None);
    }

    // ----------------------------------------------------------- the replies

    #[test]
    fn a_reply_becomes_the_function_it_describes() {
        let value = serde_json::json!({
            "present": true,
            "file": "tradebot/risk.py",
            "func": "risk_gate",
            "first": 92,
            "lines": ["def risk_gate():", "    pass"],
        });
        match read(&value) {
            Answer::Have(source) => {
                assert_eq!(source.func, "risk_gate");
                assert_eq!(source.first, 92);
                assert_eq!(source.lines.len(), 2);
            }
            _ => panic!("that was a function"),
        }
    }

    #[test]
    fn a_reply_with_no_code_carries_the_reason_the_card_should_print() {
        let value = serde_json::json!({"present": false, "reason": "ไม่พบไฟล์"});
        match read(&value) {
            Answer::Missing(reason) => assert_eq!(reason, "ไม่พบไฟล์"),
            _ => panic!("that was a refusal"),
        }
    }

    #[test]
    fn a_reply_missing_its_first_line_does_not_shift_every_number_by_one() {
        let value = serde_json::json!({"present": true, "lines": ["x = 1"]});
        match read(&value) {
            Answer::Have(source) => assert_eq!(source.first, 1),
            _ => panic!("still a function"),
        }
    }

    #[test]
    fn a_file_name_travels_as_a_query_string_without_losing_its_slashes() {
        assert_eq!(
            encode("tradebot/core/engine.py"),
            "tradebot/core/engine.py",
            "nothing in a normal name needs escaping"
        );
        assert_eq!(encode("a b&c=d"), "a%20b%26c%3Dd");
    }

    // ---------------------------------------------- the plan from the config

    #[test]
    fn the_five_functions_arrive_in_stage_order() {
        let value = serde_json::json!({
            "present": true,
            "stages": [
                {"file": "tradebot/brokers/grvt.py", "func": "fetch_mark_price", "line": 300},
                {"file": "tradebot/strategies/ping_pong.py", "func": "get_signal", "line": 77},
                {"file": "tradebot/risk.py", "func": "risk_gate", "line": 91},
                {"file": "tradebot/brokers/grvt.py", "func": "place_order", "line": 360},
                {"file": "tradebot/core/engine.py", "func": "_close", "line": 413},
            ],
        });
        let plan = read_stages(&value);
        assert_eq!(plan.len(), 5);
        assert_eq!(plan[2].func, "risk_gate");
        assert_eq!(plan[2].line, 91, "keyed by the line its def is on");
    }

    #[test]
    fn a_stage_the_server_could_not_work_out_does_not_shift_the_others() {
        let value = serde_json::json!({
            "stages": [
                {"file": "", "func": "", "line": 1},
                {"file": "tradebot/strategies/ping_pong.py", "func": "get_signal", "line": 77},
            ],
        });
        let mut code = Code::default();
        code.hold_plan(read_stages(&value));
        assert!(code.planned(0).is_none(), "ขั้นที่ไม่รู้ ต้องว่าง");
        assert_eq!(
            code.planned(1).map(|call| call.func.as_str()),
            Some("get_signal"),
            "ขั้นที่เหลือต้องไม่เลื่อน"
        );
    }

    #[test]
    fn a_plan_the_server_could_not_work_out_is_an_empty_one_not_a_crash() {
        assert!(read_stages(&serde_json::json!({"present": false})).is_empty());
        assert!(read_stages(&serde_json::json!({"stages": "nonsense"})).is_empty());
        assert!(read_stages(&serde_json::json!([])).is_empty());
    }

    #[test]
    fn the_head_of_a_function_skips_a_docstring_and_a_wrapped_signature() {
        let source = Source {
            file: "x.py".into(),
            func: "place_order".into(),
            first: 452,
            lines: vec![
                "    def place_order(".into(),
                "        self, order: SizedOrder".into(),
                "    ) -> OrderReceipt:".into(),
                format!("        {DOUBLE_DOC}Send an approved order as a limit."),
                "".into(),
                format!("        Limit, not market.{DOUBLE_DOC}"),
                "        client = self._require_client()".into(),
                "        return client.send(order)".into(),
            ],
        };
        let head: Vec<usize> = source.head(2).into_iter().map(|(n, _, _)| n).collect();
        assert_eq!(head, vec![458, 459], "straight to the first line of code");
    }

    #[test]
    fn a_one_line_docstring_is_skipped_as_one_line() {
        let source = Source {
            file: "x.py".into(),
            func: "f".into(),
            first: 1,
            lines: vec![
                "def f():".into(),
                format!("    {SINGLE_DOC}Return one.{SINGLE_DOC}"),
                "    return 1".into(),
            ],
        };
        assert_eq!(source.head(1)[0].0, 3);
    }

    #[test]
    fn a_function_with_no_docstring_starts_right_after_its_signature() {
        let source = Source {
            file: "x.py".into(),
            func: "f".into(),
            first: 10,
            lines: vec!["def f(x):".into(), "    return x".into()],
        };
        assert_eq!(source.head(5)[0].0, 11);
    }

    #[test]
    fn the_head_of_a_function_skips_the_signature_the_card_already_prints() {
        let source = source();
        let head: Vec<usize> = source.head(2).into_iter().map(|(n, _, _)| n).collect();
        assert_eq!(head, vec![93, 94]);
        assert!(source.head(99).iter().all(|(_, _, hits)| *hits == 0));
    }

    #[test]
    fn only_the_steps_inside_this_function_belong_to_this_card() {
        // The tick went into risk_gate, out to a helper and back.
        let walk = vec![92, 93, 10, 11, 95];
        assert_eq!(source().inside(&walk), vec![92, 93, 95]);
    }

    #[test]
    fn a_step_that_repeats_stays_repeated() {
        // Two trips round a loop is two steps, not one: that is the whole
        // difference between a path and a set of lines.
        let walk = vec![93, 94, 93, 94];
        assert_eq!(source().inside(&walk), walk);
    }

    #[test]
    fn the_window_travels_with_the_line_being_walked() {
        let hits = BTreeMap::new();
        let source = source();
        let top: Vec<usize> = source
            .around(92, 2, &hits)
            .into_iter()
            .map(|(n, _, _)| n)
            .collect();
        assert_eq!(top, vec![92, 93], "ติดขอบบนแล้วไม่เลื่อนต่อ");
        let middle: Vec<usize> = source
            .around(94, 2, &hits)
            .into_iter()
            .map(|(n, _, _)| n)
            .collect();
        assert_eq!(middle, vec![93, 94]);
        let bottom: Vec<usize> = source
            .around(95, 2, &hits)
            .into_iter()
            .map(|(n, _, _)| n)
            .collect();
        assert_eq!(bottom, vec![94, 95], "ติดขอบล่างแล้วไม่เลยออกไป");
    }
}
