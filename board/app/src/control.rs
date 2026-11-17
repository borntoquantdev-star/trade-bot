//! The control panel: start and stop the bot, check a strategy and put the
//! exchange keys in, from the board instead of from command-line flags.
//!
//! The panel exists only where the board's own server offers it. That is the
//! student's laptop, where `board-server` is the thing that spawns the bot;
//! on a deployed board the routes do not exist, `GET /api/control` answers
//! 404, and the card (and its nav button) never appear. So the first thing
//! this module learns is *whether it is there at all*, and a 404 is that
//! answer - not an error to put on screen.
//!
//! There is deliberately no real-money option here. The two modes are paper
//! and testnet; `--live` against production is a separate, conscious step
//! that does not belong behind a button.
//!
//! The keys typed into this panel are write-only. The server never sends a
//! value back (only "is it set"), and this module never prints, logs or
//! formats what was typed: the fields are drawn masked, and the form's
//! `Debug` says nothing about its contents.

use std::sync::mpsc::{channel, Receiver, Sender};

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Rect, RichText, Ui, UiBuilder};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{api, theme};

/// The header every POST must carry. The server refuses a state-changing
/// request without it, which is what stops a web page the student happens
/// to have open from starting their bot with a form submit.
pub const HEADER: &str = "x-board-control";

/// How often to ask while something is happening (a job running, the
/// programs being installed), in seconds.
const BUSY_EVERY: f64 = 1.0;
/// ... and while nothing is.
const IDLE_EVERY: f64 = 3.0;
/// After a failed request: not so soon that a dead server is hammered.
const RETRY_AFTER_ERROR: f64 = 5.0;
/// After a 404. The routes are absent on this server and will stay absent,
/// so this is a courtesy check rather than a poll.
const RETRY_WHEN_ABSENT: f64 = 30.0;
/// How long the answer to a button press stays on screen.
const NOTE_FOR: f64 = 12.0;
/// How many lines of output are drawn. The server already sends only the
/// tail; this is a ceiling on what one frame has to lay out.
const SHOWN_LINES: usize = 300;

const OUTPUT_FILL: Color32 = Color32::from_rgb(0x0f, 0x0c, 0x09);

// ------------------------------------------------------------------ the data

/// Is there a control panel on the other end?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// Not asked yet, or the answer was lost. Drawn as absent.
    Unknown,
    Available,
    /// A 404 (or a reply that is not the control API at all): this is a
    /// deployed board.
    Unavailable,
}

/// What to run the bot against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Simulated in the bot's own process. No exchange, no money.
    #[default]
    Paper,
    /// GRVT's test network: real orders on a real order book, play money.
    Testnet,
}

impl Mode {
    pub const ALL: [Mode; 2] = [Mode::Paper, Mode::Testnet];

    /// The word the server's routes use.
    pub fn wire(self) -> &'static str {
        match self {
            Mode::Paper => "paper",
            Mode::Testnet => "testnet",
        }
    }

    pub fn from_wire(text: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|mode| mode.wire() == text)
    }
}

/// What a mode is called to a person, given the server's word for it.
fn mode_label(wire: &str) -> String {
    match Mode::from_wire(wire) {
        Some(Mode::Paper) => "กระดาษ".to_owned(),
        Some(Mode::Testnet) => "testnet".to_owned(),
        // A mode this board has never heard of is shown as it came rather
        // than as one it does know.
        None => wire.to_owned(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SetupState {
    Ready,
    Installing,
    Failed,
    Missing,
    /// A state from a newer server. Not ready, so nothing can be started.
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Setup {
    #[serde(default)]
    pub state: SetupState,
    #[serde(default)]
    pub detail: String,
}

/// The newest job the server started: a run, a check or an unlock.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Job {
    /// `run`, `check` or `resume`.
    #[serde(default)]
    pub action: String,
    /// `paper` or `testnet`.
    #[serde(default)]
    pub mode: String,
    /// The strategy it was asked to run, or `None` for the config's own.
    #[serde(default)]
    pub strategy: Option<String>,
    #[serde(default)]
    pub started_at: String,
    #[serde(default)]
    pub running: bool,
    #[serde(default)]
    pub exit_code: Option<i64>,
    /// A polite stop has been asked for: the bot is closing its position
    /// and will exit by itself.
    #[serde(default)]
    pub stopping: bool,
}

/// Which keys the server has - never what they are.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct KeysSet {
    #[serde(default)]
    pub api_key: bool,
    #[serde(default)]
    pub private_key: bool,
    #[serde(default)]
    pub sub_account_id: bool,
}

impl KeysSet {
    /// Enough to download candles and talk to the testnet: the two that
    /// sign and identify the requests.
    pub fn can_fetch(self) -> bool {
        self.api_key && self.private_key
    }
}

/// Which modes the bot has locked itself out of.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct Halted {
    #[serde(default)]
    pub paper: bool,
    #[serde(default)]
    pub testnet: bool,
}

impl Halted {
    pub fn of(self, mode: Mode) -> bool {
        match mode {
            Mode::Paper => self.paper,
            Mode::Testnet => self.testnet,
        }
    }
}

/// What `tradebot --check` last said about a strategy, for testnet.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StrategyCheck {
    #[serde(default)]
    pub passed: bool,
    #[serde(default)]
    pub why: String,
}

/// One strategy the bot knows, as the server relays it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StrategyRow {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub file: String,
    /// `None`: the bot could not say (its config could not be read).
    #[serde(default)]
    pub check: Option<StrategyCheck>,
}

/// The server's last listing of the bot's strategies. Absent from an answer
/// until the first listing is in.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Strategies {
    #[serde(default)]
    pub list: Vec<StrategyRow>,
    /// Why the latest listing failed, if it did. The rows are the last good
    /// ones.
    #[serde(default)]
    pub error: Option<String>,
}

/// Has a strategy passed the check that stands in front of testnet?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    Passed,
    NotPassed,
    Unknown,
}

impl CheckState {
    /// The word, and the colour of it: green, amber, grey. Never the accent:
    /// that colour is the board's own, not a verdict.
    fn word(self) -> (&'static str, Color32) {
        match self {
            CheckState::Passed => ("ผ่านด่านแล้ว", theme::OK),
            CheckState::NotPassed => ("ยังไม่ผ่าน", theme::WARN),
            CheckState::Unknown => ("ไม่รู้", theme::FAINT),
        }
    }
}

impl StrategyRow {
    pub fn check_state(&self) -> CheckState {
        match &self.check {
            Some(check) if check.passed => CheckState::Passed,
            Some(_) => CheckState::NotPassed,
            None => CheckState::Unknown,
        }
    }
}

/// The two worked examples, in the order students meet them: the name the
/// bot uses, what to call it, and the line under it.
const CASE_STUDIES: [(&str, &str, &str); 2] = [
    (
        "ping_pong",
        "กรณีศึกษา 1",
        "เครื่องทดสอบ เข้าออกถี่ให้เห็นกลไก ไม่ใช่กลยุทธ์ทำเงิน",
    ),
    (
        "ma_cross",
        "กรณีศึกษา 2",
        "ตามเทรนด์ เส้นค่าเฉลี่ยเร็วตัดเส้นช้า อาจรอหลายนาทีกว่าจะมีไม้",
    ),
];

/// Only there to prove the plumbing works; never offered.
const HIDDEN_STRATEGY: &str = "random";

/// Strategies of the student's own that are listed before the rest are
/// summed up in a line, so the card cannot grow without end.
const MAX_OWN_SHOWN: usize = 5;

/// What to say when the student has written none yet.
const OWN_HINT: &str = "อยากได้กลยุทธ์ของตัวเอง? คุยกับ AI ด้วย /bot:strategy แล้วมันจะโผล่ในรายการนี้ \
                        ต้องผ่าน /bot:check หรือปุ่มตรวจก่อนรัน testnet";

/// One line of the picker, ready to draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickRow {
    pub name: String,
    pub title: String,
    pub detail: String,
    pub state: CheckState,
    pub why: String,
}

/// The picker's rows: the case studies first, the student's own after, the
/// plumbing test not at all. Also how many of the student's own were left
/// out for want of room.
pub fn pick_rows(strategies: &Strategies) -> (Vec<PickRow>, usize) {
    let mut rows = Vec::new();
    for (name, label, detail) in CASE_STUDIES {
        if let Some(row) = strategies.list.iter().find(|row| row.name == name) {
            rows.push(PickRow {
                name: row.name.clone(),
                title: format!("{label} · {name}"),
                detail: detail.to_owned(),
                state: row.check_state(),
                why: row
                    .check
                    .as_ref()
                    .map(|c| c.why.clone())
                    .unwrap_or_default(),
            });
        }
    }
    let own: Vec<&StrategyRow> = strategies
        .list
        .iter()
        .filter(|row| {
            row.name != HIDDEN_STRATEGY && !CASE_STUDIES.iter().any(|case| case.0 == row.name)
        })
        .collect();
    let left_out = own.len().saturating_sub(MAX_OWN_SHOWN);
    for row in own.into_iter().take(MAX_OWN_SHOWN) {
        let description: String = row.description.chars().take(90).collect();
        let detail = [description.as_str(), row.file.as_str()]
            .iter()
            .filter(|part| !part.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" · ");
        rows.push(PickRow {
            name: row.name.clone(),
            title: format!("ของคุณ · {}", row.name),
            detail,
            state: row.check_state(),
            why: row
                .check
                .as_ref()
                .map(|c| c.why.clone())
                .unwrap_or_default(),
        });
    }
    (rows, left_out)
}

/// One answer from `GET /api/control`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Snapshot {
    #[serde(default)]
    pub setup: Setup,
    #[serde(default)]
    pub job: Option<Job>,
    #[serde(default)]
    pub output: Vec<String>,
    #[serde(default)]
    pub keys: KeysSet,
    #[serde(default)]
    pub halted: Halted,
    /// `None` until the server has listed the strategies once.
    #[serde(default)]
    pub strategies: Option<Strategies>,
}

impl Snapshot {
    /// Read a reply body, or `None` if it is not the control API's answer.
    ///
    /// A deployed board's server may answer an unknown path with its own
    /// front page and a 200; that must not make a control panel appear, so
    /// the one thing demanded is the `setup` object.
    pub fn parse(body: &str) -> Option<Snapshot> {
        let value: Value = serde_json::from_str(body).ok()?;
        if !value.get("setup").is_some_and(Value::is_object) {
            return None;
        }
        serde_json::from_value(value).ok()
    }

    fn running_job(&self) -> Option<&Job> {
        self.job.as_ref().filter(|job| job.running)
    }

    fn busy(&self) -> bool {
        self.running_job().is_some() || self.setup.state == SetupState::Installing
    }
}

/// The three key fields while somebody is typing in them.
#[derive(Default)]
pub struct Form {
    api_key: String,
    private_key: String,
    sub_account_id: String,
    /// Bumped each time the fields are cleared. It is part of each field's
    /// id, so a cleared field is a new widget with no undo history: Ctrl+Z
    /// in it cannot bring a saved key back.
    generation: u32,
}

/// Says nothing about what is inside, so no `{:?}` anywhere can leak it.
impl std::fmt::Debug for Form {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Form(..)")
    }
}

impl Form {
    pub fn is_empty(&self) -> bool {
        self.api_key.trim().is_empty()
            && self.private_key.trim().is_empty()
            && self.sub_account_id.trim().is_empty()
    }

    /// The body of the save request: only the fields somebody filled in,
    /// since the server reads a missing field as "leave it as it is".
    fn body(&self) -> Value {
        let mut map = serde_json::Map::new();
        for (name, value) in [
            ("api_key", &self.api_key),
            ("private_key", &self.private_key),
            ("sub_account_id", &self.sub_account_id),
        ] {
            // A key pasted from a page brings its newline with it.
            let value = value.trim();
            if !value.is_empty() {
                map.insert(name.to_owned(), Value::String(value.to_owned()));
            }
        }
        Value::Object(map)
    }

    fn clear(&mut self) {
        self.api_key = String::new();
        self.private_key = String::new();
        self.sub_account_id = String::new();
        self.generation = self.generation.wrapping_add(1);
    }
}

// ------------------------------------------------------------- what to press

/// The things a button can ask the server to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Run,
    Stop,
    Check,
    Resume,
    Keys,
}

impl Kind {
    pub fn path(self) -> &'static str {
        match self {
            Kind::Run => "/api/control/run",
            Kind::Stop => "/api/control/stop",
            Kind::Check => "/api/control/check",
            Kind::Resume => "/api/control/resume",
            Kind::Keys => "/api/control/keys",
        }
    }
}

/// A POST to the control routes, with the header the server insists on.
pub fn post(path: &str, body: &Value) -> ehttp::Request {
    let mut request =
        ehttp::Request::post(api::url(path), serde_json::to_vec(body).unwrap_or_default());
    // `Request::post` says the body is plain text; it is JSON.
    request.headers = ehttp::Headers::new(&[
        ("Accept", "*/*"),
        ("Content-Type", "application/json"),
        (HEADER, "1"),
    ]);
    request
}

/// Which buttons may be pressed right now: the ones that would not be
/// answered with a 409.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gates {
    pub start: bool,
    pub stop: bool,
    pub check: bool,
    pub resume: bool,
    /// The mode cannot be changed under a bot that is running in one.
    pub pick_mode: bool,
}

/// The three things a first-timer does to get a strategy onto testnet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Keys,
    Check,
    Start,
}

/// The steps in order, with what each is called on screen. Numbers, not
/// arrows: the board's fonts have no arrow glyph.
const STEPS: [(Step, &str); 3] = [
    (Step::Keys, "1 ใส่กุญแจ"),
    (Step::Check, "2 ตรวจกลยุทธ์"),
    (Step::Start, "3 เริ่มบอท"),
];

/// What the paper mode says instead of steps: it needs none.
const PAPER_GUIDE: &str = "กระดาษ: กดเริ่มได้เลย ไม่ต้องใช้กุญแจ";

/// The step guide as one line of text with three colours: finished steps in
/// green, the step to do now in the accent colour and marked in words (not
/// only colour), the rest dim.
fn guide_job(now: Step) -> LayoutJob {
    let at = |step: Step| {
        STEPS
            .iter()
            .position(|(each, _)| *each == step)
            .unwrap_or(0)
    };
    let format = |color: Color32| TextFormat {
        font_id: FontId::proportional(11.5),
        color,
        ..Default::default()
    };
    let mut job = LayoutJob::default();
    job.append("ขั้นตอนสำหรับ testnet · ", 0.0, format(theme::FAINT));
    for (index, (step, label)) in STEPS.iter().enumerate() {
        if index > 0 {
            job.append(" · ", 0.0, format(theme::FAINT));
        }
        if *step == now {
            job.append(&format!("{label} (ตอนนี้)"), 0.0, format(theme::ACCENT));
        } else if index < at(now) {
            job.append(label, 0.0, format(theme::OK));
        } else {
            job.append(label, 0.0, format(theme::DIM));
        }
    }
    job
}

/// What the bot printed when it refused to trade for want of a passed
/// `--check` (tradebot/cli/main.py, `_cleared`): "not sent: ..." and
/// "run the check first ...".
pub fn refused_for_check(output: &[String]) -> bool {
    output.iter().any(|line| {
        let line = line.to_lowercase();
        line.contains("not sent:") || line.contains("run the check first")
    })
}

/// What a check that did not pass should tell a beginner to do first.
///
/// The check is two steps on the server: download candles (which needs the
/// testnet keys), then check against them. Without keys the download is
/// skipped, the server says so ("ยังไม่มีกุญแจ testnet"), and the check
/// reports that no candles are stored ("none stored for ... Fetch them:",
/// tradebot/cli/check.py). All of those mean the same thing to a student:
/// the keys are missing, and the strategy has not been judged yet.
pub fn check_needs_keys(output: &[String], keys: KeysSet) -> bool {
    !keys.can_fetch()
        || output.iter().any(|line| {
            line.to_lowercase().contains("none stored for") || line.contains("ยังไม่มีกุญแจ testnet")
        })
}

/// The hint under a check that stopped for want of keys.
const KEYS_HINT: &str = "ใส่กุญแจ testnet ด้านขวาก่อน แล้วกดตรวจอีกครั้ง";

/// How a finished job ended, as a sentence and a colour.
///
/// `keys` and `output` are the snapshot's: a check that did not pass is
/// explained differently depending on whether it was ever able to run.
pub fn result_line(job: &Job, keys: KeysSet, output: &[String]) -> Option<(String, Color32)> {
    if job.running {
        return None;
    }
    if job.action == "check" && job.exit_code != Some(0) && check_needs_keys(output, keys) {
        return Some((KEYS_HINT.to_owned(), theme::WARN));
    }
    let started = crate::time::hhmm(&job.started_at);
    let at = if started.is_empty() {
        String::new()
    } else {
        format!(" · เริ่ม {started}")
    };
    let mode = mode_label(&job.mode);
    Some(match (job.action.as_str(), job.exit_code) {
        ("check", Some(0)) => (format!("ตรวจผ่าน · กลยุทธ์นี้ไปต่อที่ testnet ได้{at}"), theme::OK),
        ("check", Some(_)) => (
            format!("ตรวจไม่ผ่าน · อ่านเหตุผลในกล่องข้อความข้างล่าง{at}"),
            theme::BAD,
        ),
        ("check", None) => (format!("การตรวจจบโดยไม่มีผล{at}"), theme::WARN),
        ("resume", Some(0)) => (format!("ปลดล็อกแล้ว ({mode}){at}"), theme::OK),
        ("resume", Some(code)) => (format!("ปลดล็อกไม่สำเร็จ · รหัสออก {code}{at}"), theme::BAD),
        ("resume", None) => (format!("การปลดล็อกจบโดยไม่มีผล{at}"), theme::WARN),
        (_, Some(0)) => (
            format!("รอบล่าสุด ({mode}) จบแล้ว · รหัสออก 0{at}"),
            theme::MUTED,
        ),
        (_, Some(code)) => (
            format!("รอบล่าสุด ({mode}) จบแล้ว · รหัสออก {code}{at}"),
            theme::WARN,
        ),
        (_, None) => (format!("รอบล่าสุด ({mode}) จบแล้ว{at}"), theme::MUTED),
    })
}

/// What a button press came to, shown for a few seconds.
#[derive(Debug, Clone)]
struct Note {
    kind: Kind,
    text: String,
    good: bool,
    at: f64,
}

// --------------------------------------------------------------- the fetching

struct Reply {
    status: u16,
    body: String,
}

impl Reply {
    fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

enum Got {
    Status(Result<Reply, String>),
    Done(Kind, Result<Reply, String>),
}

fn answer(result: Result<ehttp::Response, String>) -> Result<Reply, String> {
    let response = result?;
    Ok(Reply {
        status: response.status,
        body: response.text().unwrap_or_default().to_owned(),
    })
}

/// The sentence a failed request's body offers, if it offers one.
fn reason_in(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    ["error", "detail", "message", "reason"]
        .iter()
        .find_map(|key| value.get(*key)?.as_str())
        .map(|text| text.chars().take(200).collect())
}

pub struct Control {
    availability: Availability,
    /// The last good answer. Kept when a later request fails, so the card
    /// goes grey instead of going blank.
    snapshot: Snapshot,
    /// Did the latest request get through?
    link_ok: bool,
    mode: Mode,
    form: Form,
    /// Did the strategy pass its check? Only the newest job is in an
    /// answer, so this remembers what an earlier answer showed: a check that
    /// passed is still a check that passed once a run has taken its place.
    /// `None` until a job has said.
    checked: Option<bool>,
    /// The strategy picked in the list. Only a request to start or check
    /// sends it; see `selected`.
    pick: Option<String>,
    note: Option<Note>,
    tx: Sender<Got>,
    rx: Receiver<Got>,
    due: f64,
    flight: Option<f64>,
    post: Option<(Kind, f64)>,
}

impl Default for Control {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            availability: Availability::Unknown,
            snapshot: Snapshot::default(),
            link_ok: true,
            mode: Mode::default(),
            form: Form::default(),
            checked: None,
            pick: None,
            note: None,
            tx,
            rx,
            due: 0.0,
            flight: None,
            post: None,
        }
    }
}

impl Control {
    pub fn availability(&self) -> Availability {
        self.availability
    }

    /// Is the panel to be drawn at all?
    pub fn available(&self) -> bool {
        self.availability == Availability::Available
    }

    /// Feed in an answer to the status request, as if the network had
    /// brought it - for the board's own tests, which have no server.
    #[cfg(test)]
    pub(crate) fn hear(&mut self, status: u16, body: &str) {
        self.accept(
            Ok(Reply {
                status,
                body: body.to_owned(),
            }),
            1.0,
        );
    }

    /// Ask the server, on the clock the state calls for. Cheap enough to
    /// call every frame.
    pub fn poll(&mut self, now: f64) {
        self.drain(now);
        if api::still_waiting(self.flight, now) || now < self.due {
            return;
        }
        self.flight = Some(now);
        let tx = self.tx.clone();
        ehttp::fetch(
            ehttp::Request::get(api::url("/api/control")),
            move |result| {
                let _ = tx.send(Got::Status(answer(result)));
            },
        );
    }

    fn drain(&mut self, now: f64) {
        while let Ok(got) = self.rx.try_recv() {
            match got {
                Got::Status(reply) => {
                    self.flight = None;
                    self.accept(reply, now);
                }
                Got::Done(kind, reply) => {
                    self.post = None;
                    self.finish(kind, reply, now);
                }
            }
        }
    }

    /// Take an answer to the status request, and decide when to ask next.
    fn accept(&mut self, reply: Result<Reply, String>, now: f64) {
        match reply {
            Ok(reply) if reply.status == 404 => {
                self.availability = Availability::Unavailable;
                self.link_ok = true;
                self.due = now + RETRY_WHEN_ABSENT;
            }
            Ok(reply) if reply.ok() => match Snapshot::parse(&reply.body) {
                Some(snapshot) => {
                    self.snapshot = snapshot;
                    self.learn_check();
                    self.availability = Availability::Available;
                    self.link_ok = true;
                    self.due = now
                        + if self.snapshot.busy() {
                            BUSY_EVERY
                        } else {
                            IDLE_EVERY
                        };
                }
                None if self.availability == Availability::Available => {
                    // It was the control API a moment ago; a garbled answer
                    // is a hiccup, not the end of the panel.
                    self.link_ok = false;
                    self.due = now + RETRY_AFTER_ERROR;
                }
                None => {
                    self.availability = Availability::Unavailable;
                    self.due = now + RETRY_WHEN_ABSENT;
                }
            },
            Ok(_) | Err(_) => {
                self.link_ok = false;
                self.due = now + RETRY_AFTER_ERROR;
            }
        }
    }

    /// Take from the newest job what it says about the strategy's check.
    fn learn_check(&mut self) {
        let Some(job) = self.snapshot.job.as_ref() else {
            return;
        };
        let testnet = job.mode == "testnet";
        let refused = refused_for_check(&self.snapshot.output);
        match (job.action.as_str(), job.running) {
            ("check", false) => self.checked = Some(job.exit_code == Some(0)),
            // A testnet run that is going was let through, which only a
            // passed check does.
            ("run", true) if testnet => self.checked = Some(true),
            ("run", false) if testnet && refused => self.checked = Some(false),
            ("run", false) if testnet && job.exit_code == Some(0) => self.checked = Some(true),
            _ => {}
        }
    }

    /// Where a first-timer is: the step to do now, worked out from the
    /// keys the server holds and what the newest jobs said.
    pub fn step(&self) -> Step {
        if !self.snapshot.keys.can_fetch() {
            return Step::Keys;
        }
        let checking = self
            .snapshot
            .running_job()
            .is_some_and(|job| job.action == "check");
        if checking {
            return Step::Check;
        }
        // With the strategies listed, the question is about the one picked.
        // Without (an older server, or the first moments), the newest jobs
        // are all there is to go on.
        let passed = match self.selected_row() {
            Some(row) => row.check_state() == CheckState::Passed,
            None => self.checked == Some(true),
        };
        if passed {
            Step::Start
        } else {
            Step::Check
        }
    }

    /// The rows the picker offers, if the server has listed any.
    fn offered(&self) -> Vec<PickRow> {
        self.snapshot
            .strategies
            .as_ref()
            .map(|strategies| pick_rows(strategies).0)
            .unwrap_or_default()
    }

    /// The strategy the next start or check is about: what the running job
    /// runs; else what was picked, if it is still offered; else the first
    /// case study (or whatever comes first). `None` when nothing is listed,
    /// in which case no `strategy` is sent and the config decides.
    pub fn selected(&self) -> Option<String> {
        let rows = self.offered();
        if let Some(name) = self
            .snapshot
            .running_job()
            .and_then(|job| job.strategy.clone())
        {
            return Some(name);
        }
        if let Some(name) = self
            .pick
            .as_ref()
            .filter(|name| rows.iter().any(|row| &row.name == *name))
        {
            return Some(name.clone());
        }
        rows.iter()
            .find(|row| row.name == CASE_STUDIES[0].0)
            .or(rows.first())
            .map(|row| row.name.clone())
    }

    fn selected_row(&self) -> Option<&StrategyRow> {
        let name = self.selected()?;
        self.snapshot
            .strategies
            .as_ref()?
            .list
            .iter()
            .find(|row| row.name == name)
    }

    /// What testnet will say about the selection: the check state, if there
    /// is a selection to ask about.
    pub fn selected_state(&self) -> Option<(String, CheckState)> {
        let row = self.selected_row()?;
        Some((row.name.clone(), row.check_state()))
    }

    /// How the newest finished job ended, in words.
    fn result(&self) -> Option<(String, Color32)> {
        let job = self.snapshot.job.as_ref()?;
        result_line(job, self.snapshot.keys, &self.snapshot.output)
    }

    /// Send a button's request. One at a time: a second press while the
    /// first is in the air would only be answered with a 409.
    fn send(&mut self, kind: Kind, body: Value, now: f64) {
        if api::still_waiting(self.post.map(|(_, at)| at), now) {
            return;
        }
        self.post = Some((kind, now));
        self.note = None;
        let tx = self.tx.clone();
        ehttp::fetch(post(kind.path(), &body), move |result| {
            let _ = tx.send(Got::Done(kind, answer(result)));
        });
    }

    /// Take the answer to a button press.
    fn finish(&mut self, kind: Kind, reply: Result<Reply, String>, now: f64) {
        let (text, good) = match reply {
            Ok(reply) if reply.ok() => {
                // Whatever that did, look at the result straight away
                // instead of up to three seconds from now.
                self.due = now;
                match kind {
                    Kind::Keys => {
                        // Out of the fields the moment the server has them.
                        self.form.clear();
                        (
                            "บันทึกกุญแจแล้ว · ช่องถูกล้างแล้ว กุญแจอยู่ในไฟล์ config\\.env".to_owned(),
                            true,
                        )
                    }
                    Kind::Stop => ("ส่งคำสั่งหยุดแล้ว · บอทกำลังปิดไม้ก่อนออก".to_owned(), true),
                    Kind::Run | Kind::Check | Kind::Resume => (String::new(), true),
                }
            }
            Ok(reply) if reply.status == 409 => {
                self.due = now;
                let why = reason_in(&reply.body)
                    .unwrap_or_else(|| "บอทกำลังทำงานอยู่ หรือโปรแกรมยังไม่พร้อม".to_owned());
                (format!("ตอนนี้ทำไม่ได้ · {why}"), false)
            }
            Ok(reply) => {
                let why = reason_in(&reply.body)
                    .map(|why| format!(" · {why}"))
                    .unwrap_or_default();
                (
                    format!("ไม่สำเร็จ · เซิร์ฟเวอร์ตอบ {}{why}", reply.status),
                    false,
                )
            }
            Err(_) => ("ส่งคำสั่งไม่ถึงเซิร์ฟเวอร์ · ลองใหม่อีกครั้ง".to_owned(), false),
        };
        self.note = if text.is_empty() {
            None
        } else {
            Some(Note {
                kind,
                text,
                good,
                at: now,
            })
        };
    }

    // ------------------------------------------------------------ what it says

    /// The mode the panel is talking about: the one the bot is running in
    /// while it runs, otherwise the one picked.
    pub fn shown_mode(&self) -> Mode {
        self.snapshot
            .running_job()
            .and_then(|job| Mode::from_wire(&job.mode))
            .unwrap_or(self.mode)
    }

    pub fn gates(&self, now: f64) -> Gates {
        let snapshot = &self.snapshot;
        let ready = snapshot.setup.state == SetupState::Ready;
        let running = snapshot.running_job();
        let posting = api::still_waiting(self.post.map(|(_, at)| at), now);
        let halted = snapshot.halted.of(self.shown_mode());
        Gates {
            // A halted bot would start and refuse to open anything, which
            // looks like a bot that is broken. Unlocking comes first.
            start: ready && running.is_none() && !posting && !halted,
            stop: running.is_some_and(|job| !job.stopping) && !posting,
            check: ready && running.is_none() && !posting,
            resume: ready && running.is_none() && !posting && halted,
            pick_mode: running.is_none(),
        }
    }

    /// The card's status chip.
    pub fn state(&self) -> (theme::State, String) {
        use theme::State;
        if !self.link_ok {
            return (State::Bad, "ติดต่อเซิร์ฟเวอร์ไม่ได้".to_owned());
        }
        match self.snapshot.setup.state {
            SetupState::Installing => return (State::Wip, "กำลังเตรียม".to_owned()),
            SetupState::Ready => {}
            SetupState::Failed | SetupState::Missing | SetupState::Unknown => {
                return (State::Bad, "ใช้งานไม่ได้".to_owned());
            }
        }
        if let Some(job) = self.snapshot.running_job() {
            if job.stopping {
                return (State::Wip, "กำลังหยุด".to_owned());
            }
            return match job.action.as_str() {
                "check" => (State::Wip, "กำลังตรวจ".to_owned()),
                "resume" => (State::Wip, "กำลังปลดล็อก".to_owned()),
                _ => (
                    // Paper is a simulation, and the theme has a colour for
                    // exactly that.
                    if job.mode == "paper" {
                        State::Simulated
                    } else {
                        State::Ok
                    },
                    format!("กำลังรัน · {}", mode_label(&job.mode)),
                ),
            };
        }
        if self.snapshot.halted.of(self.mode) {
            return (State::Bad, "ล็อกอยู่".to_owned());
        }
        (State::Idle, "พร้อม".to_owned())
    }

    // -------------------------------------------------------------- the card

    /// The inside of the card.
    pub fn body(&mut self, ui: &mut Ui, now: f64) {
        self.expire_note(now);
        let whole = ui.max_rect();
        let top_down = egui::Layout::top_down(egui::Align::Min);
        // The step guide runs across the top; the two columns start under it.
        let guide = ui
            .scope_builder(UiBuilder::new().max_rect(whole).layout(top_down), |ui| {
                self.guide_line(ui)
            })
            .response
            .rect;
        let area = Rect::from_min_max(egui::pos2(whole.min.x, guide.max.y + 8.0), whole.max);
        let gap = 22.0;
        let right_width = (area.width() * 0.42).clamp(220.0, 460.0);
        let left = Rect::from_min_size(
            area.min,
            egui::vec2(area.width() - right_width - gap, area.height()),
        );
        let right = Rect::from_min_size(
            egui::pos2(area.max.x - right_width, area.min.y),
            egui::vec2(right_width, area.height()),
        );
        let used_left = ui
            .scope_builder(UiBuilder::new().max_rect(left).layout(top_down), |ui| {
                self.controls(ui, now)
            })
            .response
            .rect;
        let used_right = ui
            .scope_builder(UiBuilder::new().max_rect(right).layout(top_down), |ui| {
                self.keys_form(ui, now)
            })
            .response
            .rect;
        // The output takes whatever the two columns above it leave.
        let below = Rect::from_min_max(
            egui::pos2(area.min.x, used_left.max.y.max(used_right.max.y) + 10.0),
            area.max,
        );
        if below.height() > 48.0 {
            ui.scope_builder(UiBuilder::new().max_rect(below).layout(top_down), |ui| {
                self.output(ui)
            });
        }
    }

    /// The first-timer's guide: paper needs nothing, testnet needs keys, a
    /// passed check, then a start.
    fn guide_line(&self, ui: &mut Ui) {
        match self.shown_mode() {
            Mode::Paper => {
                ui.label(RichText::new(PAPER_GUIDE).size(11.5).color(theme::MUTED));
            }
            Mode::Testnet => {
                ui.label(guide_job(self.step()));
            }
        }
    }

    fn expire_note(&mut self, now: f64) {
        if self
            .note
            .as_ref()
            .is_some_and(|note| now - note.at > NOTE_FOR || now < note.at)
        {
            self.note = None;
        }
    }

    fn controls(&mut self, ui: &mut Ui, now: f64) {
        ui.spacing_mut().item_spacing.y = 5.0;
        if !self.link_ok {
            ui.label(
                RichText::new("ติดต่อเซิร์ฟเวอร์ของบอร์ดไม่ได้ · สิ่งที่เห็นอาจเป็นของเมื่อครู่")
                    .size(11.0)
                    .color(theme::BAD),
            );
        }
        self.banner(ui);

        let gates = self.gates(now);
        section(ui, "เลือกโหมด");
        let mut pick = self.shown_mode();
        ui.add_enabled_ui(gates.pick_mode, |ui| {
            ui.radio_value(
                &mut pick,
                Mode::Paper,
                two_lines(
                    "กระดาษ (ไม่ใช้เงิน)",
                    "จำลองในเครื่องนี้ ไม่ส่งอะไรไปที่ตลาด · เหมาะกับลองครั้งแรก",
                ),
            );
            ui.radio_value(
                &mut pick,
                Mode::Testnet,
                two_lines(
                    "testnet (เงินทดลองบนตลาดจริง)",
                    "ส่งออเดอร์ไปที่ GRVT testnet · เงินทดลอง ต้องตรวจกลยุทธ์ก่อนครั้งแรก",
                ),
            );
        });
        if gates.pick_mode {
            self.mode = pick;
        }
        ui.label(
            RichText::new("แผงนี้ไม่มีโหมดเงินจริง · ตั้งใจให้เป็นแบบนั้น")
                .size(10.0)
                .color(theme::FAINT),
        );
        ui.add_space(2.0);

        self.strategy_picker(ui, gates.pick_mode);
        ui.add_space(2.0);

        let mode = self.shown_mode();
        let stopping = self.snapshot.running_job().is_some_and(|job| job.stopping);
        let halted = self.snapshot.halted.of(mode);
        let chosen = self.selected();
        let needs_check = mode == Mode::Testnet
            && self.snapshot.running_job().is_none()
            && self
                .selected_state()
                .is_some_and(|(_, state)| state != CheckState::Passed);
        let mut sent: Option<(Kind, Value)> = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if ui
                .add_enabled(gates.start, action_button("เริ่มบอท", theme::OK))
                .clicked()
            {
                sent = Some((
                    Kind::Run,
                    with_strategy(json!({ "mode": mode.wire() }), &chosen),
                ));
            }
            let stop = if stopping {
                "กำลังหยุดและปิดไม้..."
            } else {
                "หยุดบอท"
            };
            if ui
                .add_enabled(gates.stop, action_button(stop, theme::BAD))
                .clicked()
            {
                sent = Some((Kind::Stop, json!({})));
            }
            // The button the student needs next is the one that stands out.
            let check_button = if needs_check {
                action_button("ดึงข้อมูล + ตรวจกลยุทธ์", theme::ACCENT)
            } else {
                neutral_button("ดึงข้อมูล + ตรวจกลยุทธ์")
            };
            if ui.add_enabled(gates.check, check_button).clicked() {
                sent = Some((Kind::Check, with_strategy(json!({}), &chosen)));
            }
            if halted
                && ui
                    .add_enabled(gates.resume, action_button("ปลดล็อก", theme::WARN))
                    .clicked()
            {
                sent = Some((Kind::Resume, json!({ "mode": mode.wire() })));
            }
        });
        if let Some((kind, body)) = sent {
            self.send(kind, body, now);
        }

        if needs_check {
            if let Some(text) = self.check_guidance() {
                ui.label(RichText::new(text).size(11.0).color(theme::WARN));
            }
        }
        self.status_lines(ui, mode, halted);
    }

    /// What to tell a student who picked a strategy testnet would refuse:
    /// where the check is, before the bot says no.
    fn check_guidance(&self) -> Option<String> {
        let (name, state) = self.selected_state()?;
        match state {
            CheckState::Passed => None,
            CheckState::NotPassed => Some(format!(
                "testnet ต้องให้ {name} ผ่านด่านตรวจก่อน · กด ดึงข้อมูล + ตรวจกลยุทธ์ แล้วค่อยกด เริ่มบอท"
            )),
            CheckState::Unknown => Some(format!(
                "ยังไม่รู้ว่า {name} ผ่านด่านตรวจหรือยัง · กด ดึงข้อมูล + ตรวจกลยุทธ์ ก่อนเริ่มบอทบน testnet"
            )),
        }
    }

    /// The list of strategies to run: two case studies to watch, then the
    /// student's own. Locked while a bot is running, like the mode.
    fn strategy_picker(&mut self, ui: &mut Ui, enabled: bool) {
        section(
            ui,
            "เลือกกลยุทธ์ · สถานะคือด่านตรวจของ testnet (กระดาษไม่ต้องตรวจ)",
        );
        let Some(strategies) = self.snapshot.strategies.clone() else {
            if self.snapshot.setup.state == SetupState::Ready {
                ui.label(
                    RichText::new("กำลังอ่านรายการกลยุทธ์จากบอท...")
                        .size(10.5)
                        .color(theme::DIM),
                );
            }
            return;
        };
        let (rows, left_out) = pick_rows(&strategies);
        let mut pick = self.selected();
        ui.add_enabled_ui(enabled, |ui| {
            for row in &rows {
                let response = ui.radio_value(&mut pick, Some(row.name.clone()), pick_job(row));
                if !row.why.is_empty() {
                    response.on_hover_text(&row.why);
                }
            }
        });
        if enabled {
            self.pick = pick;
        }
        let own = rows.len()
            - rows
                .iter()
                .filter(|row| CASE_STUDIES.iter().any(|case| case.0 == row.name))
                .count();
        if own == 0 {
            ui.label(RichText::new(OWN_HINT).size(10.5).color(theme::DIM));
        }
        if left_out > 0 {
            ui.label(
                RichText::new(format!(
                    "และอีก {left_out} กลยุทธ์ · อยู่ในโฟลเดอร์ tradebot/strategies"
                ))
                .size(10.5)
                .color(theme::DIM),
            );
        }
        if let Some(error) = strategies.error.as_deref() {
            ui.label(
                RichText::new(format!("อ่านรายการกลยุทธ์ล่าสุดไม่สำเร็จ · {error}"))
                    .size(10.5)
                    .color(theme::WARN),
            );
        }
    }

    /// Where the programs are: hidden when they are ready.
    fn banner(&self, ui: &mut Ui) {
        let setup = &self.snapshot.setup;
        match setup.state {
            SetupState::Ready => {}
            SetupState::Installing => {
                ui.label(
                    RichText::new("กำลังเตรียมโปรแกรมครั้งแรก (ประมาณ 1 นาที)...")
                        .size(12.0)
                        .color(theme::WARN),
                );
                if !setup.detail.is_empty() {
                    ui.label(RichText::new(&setup.detail).size(10.5).color(theme::DIM));
                }
            }
            SetupState::Failed | SetupState::Missing | SetupState::Unknown => {
                let detail = if setup.detail.is_empty() {
                    "เตรียมโปรแกรมของบอทไม่สำเร็จ"
                } else {
                    setup.detail.as_str()
                };
                ui.label(RichText::new(detail).size(12.0).color(theme::BAD));
            }
        }
    }

    /// What the bot is doing, how the last job ended, and the one-line
    /// answers to "why can't I press that".
    fn status_lines(&self, ui: &mut Ui, mode: Mode, halted: bool) {
        let job = self.snapshot.job.as_ref();
        if let Some(job) = job.filter(|job| job.running) {
            let what = match job.action.as_str() {
                "check" => "กำลังดึงแท่งเทียนและตรวจกลยุทธ์...".to_owned(),
                "resume" => "กำลังปลดล็อก".to_owned(),
                _ => format!("กำลังรัน · {}", mode_label(&job.mode)),
            };
            let what = match job.strategy.as_deref() {
                Some(name) => format!("{what} · {name}"),
                None => what,
            };
            let started = crate::time::hhmm(&job.started_at);
            let text = if started.is_empty() {
                what
            } else {
                format!("{what} · เริ่ม {started}")
            };
            ui.label(RichText::new(text).size(11.5).color(theme::OK));
        } else if let Some((text, colour)) = self.result() {
            ui.label(RichText::new(text).size(11.5).color(colour));
        }

        if halted && job.is_none_or(|job| !job.running) {
            ui.label(
                RichText::new(format!(
                    "บอทล็อกตัวเองไว้ในโหมด{} เพราะชนเพดานความเสี่ยงรอบก่อน \
                     · อ่านสาเหตุแล้วค่อยกด ปลดล็อก",
                    mode_label(mode.wire())
                ))
                .size(11.0)
                .color(theme::WARN),
            );
        }
        let refused = job.is_some_and(|job| job.action == "run" && !job.running)
            && refused_for_check(&self.snapshot.output);
        if refused {
            ui.label(
                RichText::new("ต้องกด ตรวจกลยุทธ์ ให้ผ่านก่อน แล้วค่อยกด เริ่มบอท อีกครั้ง")
                    .size(11.5)
                    .color(theme::WARN),
            );
        }
        if let Some(note) = self.note.as_ref().filter(|note| note.kind != Kind::Keys) {
            ui.label(
                RichText::new(&note.text)
                    .size(11.0)
                    .color(note_colour(note)),
            );
        }
    }

    fn keys_form(&mut self, ui: &mut Ui, now: f64) {
        ui.spacing_mut().item_spacing.y = 4.0;
        section(ui, "กุญแจ GRVT · ใส่ครั้งเดียวก็พอ");
        ui.label(
            RichText::new(
                "กุญแจเก็บไว้ในเครื่องนี้ที่ config\\.env เท่านั้น ไม่ส่งไปที่อื่น \
                 · ช่องที่เว้นว่างคือไม่เปลี่ยนค่าเดิม",
            )
            .size(10.5)
            .color(theme::DIM),
        );
        ui.add_space(4.0);

        let set = self.snapshot.keys;
        let Form {
            api_key,
            private_key,
            sub_account_id,
            generation,
        } = &mut self.form;
        let generation = *generation;
        let rows: [(&str, bool, &mut String, &str); 3] = [
            ("GRVT API key", set.api_key, api_key, "control_api_key"),
            (
                "Private key",
                set.private_key,
                private_key,
                "control_private_key",
            ),
            (
                "Sub account id",
                set.sub_account_id,
                sub_account_id,
                "control_sub_account_id",
            ),
        ];
        for (label, is_set, text, salt) in rows {
            ui.horizontal(|ui| {
                ui.label(RichText::new(label).size(11.0).color(theme::MUTED));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (word, colour) = if is_set {
                        ("ตั้งแล้ว", theme::OK)
                    } else {
                        ("ยังไม่ได้ตั้ง", theme::WARN)
                    };
                    ui.label(RichText::new(word).size(10.5).color(colour));
                });
            });
            ui.add(
                egui::TextEdit::singleline(text)
                    .password(true)
                    .id_salt((salt, generation))
                    .desired_width(f32::INFINITY)
                    // Named outright for the same reason as the buttons.
                    .background_color(OUTPUT_FILL)
                    .text_color(theme::TEXT)
                    .hint_text(
                        RichText::new(if is_set {
                            "ตั้งไว้แล้ว · เว้นว่างถ้าไม่เปลี่ยน"
                        } else {
                            "วางที่นี่"
                        })
                        .size(11.0)
                        .color(theme::FAINT),
                    ),
            );
        }

        ui.add_space(4.0);
        let posting = api::still_waiting(self.post.map(|(_, at)| at), now);
        let can_save = !self.form.is_empty() && !posting;
        let mut save = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            save = ui
                .add_enabled(can_save, action_button("บันทึกกุญแจ", theme::ACCENT))
                .clicked();
        });
        if save {
            let body = self.form.body();
            self.send(Kind::Keys, body, now);
        }
        if let Some(note) = self.note.as_ref().filter(|note| note.kind == Kind::Keys) {
            ui.label(
                RichText::new(&note.text)
                    .size(11.0)
                    .color(note_colour(note)),
            );
        }
    }

    /// The last lines the bot (or the installer) printed.
    fn output(&self, ui: &mut Ui) {
        let heading = if self.snapshot.setup.state == SetupState::Ready {
            "ข้อความจากรอบล่าสุด"
        } else {
            "ข้อความจากการเตรียมโปรแกรม"
        };
        section(ui, heading);
        let frame = Rect::from_min_size(
            ui.cursor().min,
            egui::vec2(ui.available_width(), ui.available_height()),
        );
        ui.painter().rect_filled(frame, 6, OUTPUT_FILL);
        let lines = &self.snapshot.output;
        let from = lines.len().saturating_sub(SHOWN_LINES);
        ui.scope_builder(UiBuilder::new().max_rect(frame.shrink(6.0)), |ui| {
            egui::ScrollArea::vertical()
                .id_salt("control_output")
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    if lines.is_empty() {
                        ui.label(
                            RichText::new(
                                "ยังไม่มีข้อความ · กด เริ่มบอท หรือ ดึงข้อมูล + ตรวจกลยุทธ์ แล้วที่นี่จะขึ้นให้ดู",
                            )
                            .size(10.5)
                            .color(theme::DIM),
                        );
                    }
                    for line in &lines[from..] {
                        ui.label(
                            RichText::new(line)
                                .size(10.5)
                                .monospace()
                                .color(theme::MUTED),
                        );
                    }
                });
        });
    }
}

/// A request body with the picked strategy in it, if there is one.
fn with_strategy(mut body: Value, strategy: &Option<String>) -> Value {
    if let (Some(name), Some(map)) = (strategy, body.as_object_mut()) {
        map.insert("strategy".to_owned(), Value::String(name.clone()));
    }
    body
}

/// A picker row as one widget: the name and the check state on a line, what
/// it is under it.
fn pick_job(row: &PickRow) -> LayoutJob {
    let (word, colour) = row.state.word();
    let mut job = LayoutJob::default();
    let format = |size: f32, color: Color32| TextFormat {
        font_id: FontId::proportional(size),
        color,
        ..Default::default()
    };
    job.append(&row.title, 0.0, format(12.0, theme::TEXT));
    job.append("  ·  ", 0.0, format(10.5, theme::FAINT));
    job.append(word, 0.0, format(10.5, colour));
    if !row.detail.is_empty() {
        job.append(&format!("\n{}", row.detail), 0.0, format(10.5, theme::DIM));
    }
    job
}

fn note_colour(note: &Note) -> Color32 {
    if note.good {
        theme::OK
    } else {
        theme::BAD
    }
}

fn section(ui: &mut Ui, title: &str) {
    ui.label(RichText::new(title).size(10.5).color(theme::FAINT));
}

fn action_button(text: &str, fill: Color32) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text).size(12.0).strong().color(theme::CANVAS))
        .fill(fill)
        .min_size(egui::vec2(0.0, 26.0))
}

fn neutral_button(text: &str) -> egui::Button<'static> {
    // Colours named outright: egui follows the browser's light or dark
    // setting for anything not named, and a cream label on the light
    // theme's white button cannot be read.
    egui::Button::new(RichText::new(text).size(12.0).color(theme::TEXT))
        .fill(theme::HEAD)
        .stroke(theme::line())
        .min_size(egui::vec2(0.0, 26.0))
}

/// A choice with its explanation under it, in one widget so the two lines
/// are one click target.
fn two_lines(title: &str, detail: &str) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.append(
        title,
        0.0,
        TextFormat {
            font_id: FontId::proportional(12.0),
            color: theme::TEXT,
            ..Default::default()
        },
    );
    job.append(
        &format!("\n{detail}"),
        0.0,
        TextFormat {
            font_id: FontId::proportional(10.5),
            color: theme::DIM,
            ..Default::default()
        },
    );
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(status: u16, body: &str) -> Result<Reply, String> {
        Ok(Reply {
            status,
            body: body.to_owned(),
        })
    }

    const READY: &str = r#"{
        "setup": {"state": "ready", "detail": "พร้อม"},
        "job": null,
        "output": [],
        "keys": {"api_key": true, "private_key": false, "sub_account_id": true},
        "halted": {"paper": false, "testnet": false}
    }"#;

    fn running(action: &str, mode: &str) -> String {
        format!(
            r#"{{"setup":{{"state":"ready","detail":""}},
                "job":{{"action":"{action}","mode":"{mode}","started_at":"2026-11-17T04:00:00+00:00",
                        "running":true,"exit_code":null,"stopping":false}},
                "output":["tick 1"],
                "keys":{{"api_key":true,"private_key":true,"sub_account_id":true}},
                "halted":{{"paper":false,"testnet":false}}}}"#
        )
    }

    fn finished(action: &str, mode: &str, code: i32, output: &[&str]) -> String {
        json!({
            "setup": {"state": "ready", "detail": ""},
            "job": {"action": action, "mode": mode, "started_at": "2026-11-17T04:00:00+00:00",
                    "running": false, "exit_code": code, "stopping": false},
            "output": output,
            "keys": {"api_key": true, "private_key": true, "sub_account_id": true},
            "halted": {"paper": false, "testnet": false}
        })
        .to_string()
    }

    fn available(body: &str) -> Control {
        let mut control = Control::default();
        control.accept(reply(200, body), 1.0);
        assert!(control.available(), "{body}");
        control
    }

    /// Every piece of text one frame of the panel painted.
    fn painted(control: &mut Control, now: f64) -> Vec<String> {
        let ctx = egui::Context::default();
        crate::fonts::install(&ctx);
        let mut warm = ctx.run_ui(egui::RawInput::default(), |_| {});
        warm.textures_delta.clear();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let area = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1076.0, 560.0));
            ui.scope_builder(UiBuilder::new().max_rect(area), |ui| control.body(ui, now));
        });
        output.textures_delta.clear();

        fn walk(shape: &egui::Shape, found: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => found.push(text.galley.text().to_owned()),
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|shape| walk(shape, found)),
                _ => {}
            }
        }
        let mut found = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut found);
        }
        found
    }

    /// The height the card hands `body`: the slot's 700, less the card's head and padding (30 + 10 + 10).
    const SLOT_BODY_HEIGHT: f32 = 650.0;

    /// Every piece of text one frame painted, with the y it was painted at.
    fn painted_at(control: &mut Control, now: f64) -> Vec<(String, f32)> {
        let ctx = egui::Context::default();
        crate::fonts::install(&ctx);
        let mut warm = ctx.run_ui(egui::RawInput::default(), |_| {});
        warm.textures_delta.clear();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let area =
                Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1076.0, SLOT_BODY_HEIGHT));
            ui.scope_builder(UiBuilder::new().max_rect(area), |ui| control.body(ui, now));
        });
        output.textures_delta.clear();

        fn walk(shape: &egui::Shape, found: &mut Vec<(String, f32)>) {
            match shape {
                egui::Shape::Text(text) => found.push((text.galley.text().to_owned(), text.pos.y)),
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|shape| walk(shape, found)),
                _ => {}
            }
        }
        let mut found = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut found);
        }
        found
    }

    fn screen(control: &mut Control) -> String {
        painted(control, 1.0).join(" | ")
    }

    // ------------------------------------------------- is it there at all?

    #[test]
    fn a_404_means_there_is_no_panel_and_stops_the_fast_polling() {
        let mut control = Control::default();
        assert!(!control.available(), "ยังไม่ได้ถามก็ยังไม่โชว์");
        control.accept(reply(404, "not found"), 10.0);
        assert_eq!(control.availability(), Availability::Unavailable);
        assert!(!control.available());
        assert!(
            control.due >= 10.0 + RETRY_WHEN_ABSENT,
            "404 คือไม่มีแผง ไม่ใช่ error ที่ต้องถามซ้ำทุกวินาที: {}",
            control.due
        );
    }

    #[test]
    fn a_front_page_with_a_200_is_not_a_control_panel() {
        // A deployed board may answer any path with its page.
        let mut control = Control::default();
        control.accept(reply(200, "<!doctype html><title>board</title>"), 5.0);
        assert_eq!(control.availability(), Availability::Unavailable);
        let mut other = Control::default();
        other.accept(reply(200, r#"{"hello":"world"}"#), 5.0);
        assert_eq!(other.availability(), Availability::Unavailable);
    }

    #[test]
    fn a_failed_request_before_the_first_answer_keeps_the_panel_hidden() {
        let mut control = Control::default();
        control.accept(Err("connection refused".into()), 2.0);
        assert_eq!(control.availability(), Availability::Unknown);
        assert!(!control.available());
        control.accept(reply(500, "boom"), 3.0);
        assert!(!control.available());
        assert_eq!(control.due, 3.0 + RETRY_AFTER_ERROR);
    }

    #[test]
    fn a_good_answer_shows_the_panel_and_a_later_failure_only_greys_it() {
        let mut control = available(READY);
        control.accept(Err("lost".into()), 20.0);
        assert!(control.available(), "การ์ดไม่หายไปเพราะเน็ตสะดุด");
        assert!(!control.link_ok);
        let (state, text) = control.state();
        assert_eq!(state, theme::State::Bad);
        assert!(text.contains("ติดต่อเซิร์ฟเวอร์ไม่ได้"), "{text}");
        assert!(screen(&mut control).contains("สิ่งที่เห็นอาจเป็นของเมื่อครู่"));
        control.accept(reply(200, READY), 25.0);
        assert!(control.link_ok);
    }

    #[test]
    fn the_panel_goes_away_if_the_server_starts_saying_404() {
        let mut control = available(READY);
        control.accept(reply(404, ""), 9.0);
        assert!(!control.available());
    }

    // ------------------------------------------------------- the polling

    #[test]
    fn it_asks_every_second_while_a_job_runs_and_every_three_when_idle() {
        let mut control = Control::default();
        control.accept(reply(200, &running("run", "paper")), 100.0);
        assert_eq!(control.due, 100.0 + BUSY_EVERY);

        control.accept(reply(200, READY), 200.0);
        assert_eq!(control.due, 200.0 + IDLE_EVERY);

        let installing = r#"{"setup":{"state":"installing","detail":"x"},"job":null,
            "output":[],"keys":{},"halted":{}}"#;
        control.accept(reply(200, installing), 300.0);
        assert_eq!(control.due, 300.0 + BUSY_EVERY, "ติดตั้งอยู่ก็ถามถี่");

        control.accept(reply(200, &finished("check", "testnet", 0, &[])), 400.0);
        assert_eq!(control.due, 400.0 + IDLE_EVERY, "งานจบแล้วกลับมาถามช้า");
    }

    #[test]
    fn a_button_that_worked_asks_again_at_once() {
        let mut control = available(READY);
        control.due = 500.0;
        control.finish(Kind::Run, reply(200, "{}"), 10.0);
        assert_eq!(control.due, 10.0);
    }

    // ------------------------------------------------------- the request

    #[test]
    fn every_post_carries_the_control_header_and_a_json_body() {
        let request = post("/api/control/run", &json!({"mode": "paper"}));
        assert_eq!(request.method, ehttp::Method::POST);
        assert!(request.url.ends_with("/api/control/run"), "{}", request.url);
        assert_eq!(request.headers.get(HEADER), Some("1"));
        assert_eq!(HEADER, "x-board-control");
        assert_eq!(
            request.headers.get("content-type"),
            Some("application/json")
        );
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body, json!({"mode": "paper"}));
    }

    #[test]
    fn each_button_goes_to_its_own_route() {
        assert_eq!(Kind::Run.path(), "/api/control/run");
        assert_eq!(Kind::Stop.path(), "/api/control/stop");
        assert_eq!(Kind::Check.path(), "/api/control/check");
        assert_eq!(Kind::Resume.path(), "/api/control/resume");
        assert_eq!(Kind::Keys.path(), "/api/control/keys");
        // And every one of them is built by the same function, so none
        // can forget the header.
        for kind in [Kind::Run, Kind::Stop, Kind::Check, Kind::Resume, Kind::Keys] {
            assert_eq!(post(kind.path(), &json!({})).headers.get(HEADER), Some("1"));
        }
    }

    #[test]
    fn only_paper_and_testnet_exist() {
        assert_eq!(Mode::ALL.len(), 2);
        assert_eq!(Mode::Paper.wire(), "paper");
        assert_eq!(Mode::Testnet.wire(), "testnet");
        assert_eq!(Mode::from_wire("live"), None);
        assert_eq!(Mode::from_wire("prod"), None);
        assert_eq!(Mode::from_wire("mainnet"), None);
    }

    // ----------------------------------------------------- the setup banner

    #[test]
    fn a_ready_setup_shows_no_banner() {
        let mut control = available(READY);
        let text = screen(&mut control);
        assert!(!text.contains("กำลังเตรียมโปรแกรม"), "{text}");
        assert!(!text.contains("ใช้งานไม่ได้"), "{text}");
        assert!(text.contains("เริ่มบอท"), "{text}");
    }

    #[test]
    fn installing_says_it_will_take_about_a_minute_and_shows_the_output() {
        let mut control = available(
            r#"{"setup":{"state":"installing","detail":"ติดตั้งแพ็กเกจ"},"job":null,
                "output":["Collecting requests","Installing collected packages"],
                "keys":{},"halted":{}}"#,
        );
        let text = screen(&mut control);
        assert!(
            text.contains("กำลังเตรียมโปรแกรมครั้งแรก (ประมาณ 1 นาที)..."),
            "{text}"
        );
        assert!(text.contains("Installing collected packages"), "{text}");
        assert!(text.contains("ข้อความจากการเตรียมโปรแกรม"), "{text}");
        assert_eq!(control.state(), (theme::State::Wip, "กำลังเตรียม".to_owned()));
        let gates = control.gates(1.0);
        assert!(!gates.start && !gates.check, "ยังไม่พร้อมก็กดเริ่มไม่ได้");
    }

    #[test]
    fn a_failed_setup_shows_the_servers_sentence_and_blocks_starting() {
        for state in ["failed", "missing"] {
            let mut control = available(&format!(
                r#"{{"setup":{{"state":"{state}","detail":"ไม่พบ Python ในเครื่อง"}},"job":null,
                    "output":["error: no python"],"keys":{{}},"halted":{{}}}}"#
            ));
            let text = screen(&mut control);
            assert!(text.contains("ไม่พบ Python ในเครื่อง"), "{state}: {text}");
            assert!(text.contains("error: no python"), "{state}: {text}");
            assert_eq!(control.state().0, theme::State::Bad);
            let gates = control.gates(1.0);
            assert!(!gates.start && !gates.check && !gates.resume, "{state}");
        }
    }

    #[test]
    fn a_setup_state_from_a_newer_server_is_not_treated_as_ready() {
        let control = available(
            r#"{"setup":{"state":"repairing","detail":"?"},"job":null,"output":[],
                "keys":{},"halted":{}}"#,
        );
        assert_eq!(control.snapshot.setup.state, SetupState::Unknown);
        assert!(!control.gates(1.0).start);
    }

    // ---------------------------------------------------------- the buttons

    #[test]
    fn when_idle_you_can_start_and_check_but_not_stop() {
        let control = available(READY);
        assert_eq!(
            control.gates(1.0),
            Gates {
                start: true,
                stop: false,
                check: true,
                resume: false,
                pick_mode: true
            }
        );
    }

    #[test]
    fn while_the_bot_runs_you_can_only_stop_it() {
        let control = available(&running("run", "paper"));
        assert_eq!(
            control.gates(1.0),
            Gates {
                start: false,
                stop: true,
                check: false,
                resume: false,
                pick_mode: false
            }
        );
        assert_eq!(
            control.state(),
            (theme::State::Simulated, "กำลังรัน · กระดาษ".to_owned())
        );
    }

    #[test]
    fn a_stop_already_asked_for_says_so_and_cannot_be_asked_twice() {
        let mut control = available(
            r#"{"setup":{"state":"ready","detail":""},
                "job":{"action":"run","mode":"testnet","started_at":"2026-11-17T04:00:00+00:00",
                       "running":true,"exit_code":null,"stopping":true},
                "output":[],"keys":{},"halted":{}}"#,
        );
        assert!(!control.gates(1.0).stop);
        let text = screen(&mut control);
        assert!(text.contains("กำลังหยุดและปิดไม้..."), "{text}");
        assert_eq!(control.state(), (theme::State::Wip, "กำลังหยุด".to_owned()));
    }

    #[test]
    fn a_button_already_sent_is_not_sent_again() {
        let mut control = available(READY);
        control.post = Some((Kind::Run, 5.0));
        let gates = control.gates(6.0);
        assert!(!gates.start && !gates.check && !gates.stop);
        // ... until the request is long lost.
        assert!(control.gates(5.0 + api::LOST + 1.0).start);
    }

    #[test]
    fn unlock_appears_only_for_a_halted_mode_and_explains_the_halt() {
        let halted = r#"{"setup":{"state":"ready","detail":""},"job":null,"output":[],
            "keys":{},"halted":{"paper":false,"testnet":true}}"#;
        let mut control = available(halted);
        // Paper is picked: nothing is halted there.
        assert!(!control.gates(1.0).resume);
        let text = screen(&mut control);
        assert!(!text.contains("ปลดล็อก"), "{text}");
        assert!(control.gates(1.0).start);

        control.mode = Mode::Testnet;
        let gates = control.gates(1.0);
        assert!(gates.resume, "testnet ล็อกอยู่ ต้องกดปลดล็อกได้");
        assert!(!gates.start, "บอทที่ล็อกอยู่เริ่มไปก็ไม่ทำอะไร: ปลดล็อกก่อน");
        let text = screen(&mut control);
        assert!(text.contains("ปลดล็อก"), "{text}");
        assert!(text.contains("ชนเพดานความเสี่ยง"), "{text}");
        assert_eq!(control.state(), (theme::State::Bad, "ล็อกอยู่".to_owned()));
    }

    #[test]
    fn a_running_bot_does_not_offer_to_unlock() {
        let control = available(
            r#"{"setup":{"state":"ready","detail":""},
                "job":{"action":"run","mode":"testnet","started_at":"","running":true,
                       "exit_code":null,"stopping":false},
                "output":[],"keys":{},"halted":{"paper":false,"testnet":true}}"#,
        );
        assert!(!control.gates(1.0).resume);
    }

    #[test]
    fn the_mode_shown_is_the_one_the_running_bot_is_in() {
        let mut control = available(&running("run", "testnet"));
        control.mode = Mode::Paper;
        assert_eq!(control.shown_mode(), Mode::Testnet);
    }

    // ------------------------------------------------------ results and hints

    #[test]
    fn a_check_says_pass_or_fail_from_its_exit_code() {
        let mut passed = available(&finished("check", "testnet", 0, &["VERDICT  PASS"]));
        let text = screen(&mut passed);
        assert!(text.contains("ตรวจผ่าน"), "{text}");
        assert!(!text.contains("ตรวจไม่ผ่าน"), "{text}");

        let mut failed = available(&finished("check", "testnet", 1, &["VERDICT  FAIL"]));
        let text = screen(&mut failed);
        assert!(text.contains("ตรวจไม่ผ่าน"), "{text}");
        assert!(text.contains("VERDICT  FAIL"), "{text}");
    }

    #[test]
    fn a_finished_run_says_the_exit_code() {
        let mut control = available(&finished("run", "paper", 0, &[]));
        let text = screen(&mut control);
        assert!(text.contains("รอบล่าสุด (กระดาษ) จบแล้ว · รหัสออก 0"), "{text}");
        let mut control = available(&finished("run", "testnet", 2, &[]));
        let text = screen(&mut control);
        assert!(text.contains("รหัสออก 2"), "{text}");
        let (_, colour) = control.result().unwrap();
        assert_eq!(colour, theme::WARN, "รหัสที่ไม่ใช่ 0 ไม่ควรดูเป็นสีปกติ");
    }

    #[test]
    fn a_job_still_running_has_no_result_line() {
        let control = available(&running("check", "testnet"));
        assert!(control.result().is_none());
    }

    #[test]
    fn the_bots_refusal_for_a_missing_check_becomes_a_plain_hint() {
        // The bot's own words (tradebot/cli/main.py, `_cleared`).
        let refusal = [
            "ERROR not sent: no passed --check for this strategy file. Nothing was sent to grvt.",
            "ERROR   run the check first (offline, seconds): tradebot --config config/testnet.yaml --check",
        ];
        let mut control = available(&finished("run", "testnet", 2, &refusal));
        let text = screen(&mut control);
        assert!(text.contains("ต้องกด ตรวจกลยุทธ์ ให้ผ่านก่อน"), "{text}");
    }

    #[test]
    fn the_hint_does_not_appear_without_the_refusal() {
        let mut control = available(&finished(
            "run",
            "testnet",
            1,
            &["halted - stopping the loop"],
        ));
        let text = screen(&mut control);
        assert!(!text.contains("ให้ผ่านก่อน"), "{text}");
        // Nor for a check that merely mentions it.
        let mut check = available(&finished("check", "testnet", 1, &["not sent: x"]));
        let text = screen(&mut check);
        assert!(!text.contains("ให้ผ่านก่อน"), "{text}");
    }

    #[test]
    fn the_refusal_is_recognised_however_it_is_cased() {
        assert!(refused_for_check(&["Not Sent: nope".to_owned()]));
        assert!(refused_for_check(&[
            "x".to_owned(),
            "RUN THE CHECK FIRST".to_owned()
        ]));
        assert!(!refused_for_check(&["all good".to_owned()]));
        assert!(!refused_for_check(&[]));
    }

    // ----------------------------------------------------------- the output

    #[test]
    fn the_output_is_drawn_in_order_and_empty_output_says_what_to_do() {
        let mut control = available(&running("run", "paper"));
        control.snapshot.output = vec!["first line".into(), "second line".into()];
        let painted = painted(&mut control, 1.0);
        let first = painted.iter().position(|text| text == "first line");
        let second = painted.iter().position(|text| text == "second line");
        assert!(
            matches!((first, second), (Some(a), Some(b)) if a < b),
            "{painted:?}"
        );

        let mut empty = available(READY);
        let text = screen(&mut empty);
        assert!(text.contains("ยังไม่มีข้อความ"), "{text}");
    }

    #[test]
    fn a_flood_of_output_is_capped_to_the_newest_lines() {
        let mut control = available(READY);
        control.snapshot.output = (0..SHOWN_LINES + 50)
            .map(|i| format!("line {i:04}"))
            .collect();
        let text = screen(&mut control);
        // One frame draws from the top of the scroll area (the stick to the
        // bottom takes a frame to engage), so the proof that the cap kept
        // the newest lines is where it starts: at the oldest line kept.
        assert!(!text.contains("line 0049"), "บรรทัดเก่าสุดไม่ต้องวาด");
        assert!(text.contains("line 0050"), "{text}");
    }

    // ------------------------------------------------------------- the keys

    #[test]
    fn each_key_says_whether_it_is_set_and_never_what_it_is() {
        let mut control = available(READY);
        let text = screen(&mut control);
        assert_eq!(text.matches("ตั้งแล้ว").count(), 2, "{text}");
        assert!(text.contains("ยังไม่ได้ตั้ง"), "{text}");
        assert!(text.contains("GRVT API key"), "{text}");
        assert!(text.contains("Private key"), "{text}");
        assert!(text.contains("Sub account id"), "{text}");
        assert!(text.contains("config\\.env"), "{text}");
    }

    #[test]
    fn what_is_typed_into_a_key_field_never_reaches_the_screen() {
        let mut control = available(READY);
        control.form.api_key = "SECRET-API-KEY-1234".into();
        control.form.private_key = "0xPRIVATE-DEADBEEF".into();
        control.form.sub_account_id = "9876543210".into();
        let text = screen(&mut control);
        for secret in [
            "SECRET-API-KEY-1234",
            "0xPRIVATE-DEADBEEF",
            "9876543210",
            "DEADBEEF",
        ] {
            assert!(!text.contains(secret), "กุญแจโผล่บนจอ: {text}");
        }
        assert!(text.contains('•'), "ช่องกุญแจต้องเป็นจุดบัง: {text}");
        // Nor through a debug print.
        let debug = format!("{:?}", control.form);
        assert!(!debug.contains("SECRET"), "{debug}");
    }

    #[test]
    fn the_bullet_that_masks_a_key_has_a_glyph() {
        let ctx = egui::Context::default();
        crate::fonts::install(&ctx);
        let mut warm = ctx.run_ui(egui::RawInput::default(), |_| {});
        warm.textures_delta.clear();
        ctx.fonts_mut(|fonts| {
            for font in [FontId::proportional(12.0), FontId::monospace(12.0)] {
                assert!(
                    fonts.has_glyphs(&font, "•"),
                    "ไม่มีรูปอักขระสำหรับจุดบังกุญแจใน {font:?}"
                );
            }
        });
    }

    #[test]
    fn only_the_fields_that_were_filled_in_are_sent_and_they_are_trimmed() {
        let mut form = Form::default();
        assert!(form.is_empty());
        assert_eq!(form.body(), json!({}));
        form.api_key = "  abc\n".into();
        form.sub_account_id = "   ".into();
        assert!(!form.is_empty());
        assert_eq!(form.body(), json!({"api_key": "abc"}));
        form.private_key = "0xdead".into();
        assert_eq!(
            form.body(),
            json!({"api_key": "abc", "private_key": "0xdead"})
        );
    }

    #[test]
    fn the_fields_are_emptied_once_the_server_has_the_keys() {
        let mut control = available(READY);
        control.form.api_key = "abc".into();
        control.form.private_key = "def".into();
        let before = control.form.generation;
        control.finish(Kind::Keys, reply(200, "{}"), 3.0);
        assert!(control.form.is_empty(), "บันทึกแล้วต้องล้างช่อง");
        assert_ne!(
            control.form.generation, before,
            "ช่องใหม่ต้องเป็นวิดเจ็ตใหม่ ไม่งั้น Ctrl+Z เอากุญแจกลับมา"
        );
        let note = control.note.as_ref().unwrap();
        assert!(note.good && note.text.contains("บันทึกกุญแจแล้ว"), "{note:?}");
    }

    #[test]
    fn the_fields_are_kept_when_saving_failed() {
        let mut control = available(READY);
        control.form.api_key = "abc".into();
        control.finish(Kind::Keys, reply(500, r#"{"error":"เขียนไฟล์ไม่ได้"}"#), 3.0);
        assert_eq!(
            control.form.api_key, "abc",
            "พิมพ์มายาว ไม่ควรหายเพราะเซิร์ฟเวอร์พัง"
        );
        let note = control.note.as_ref().unwrap();
        assert!(!note.good && note.text.contains("เขียนไฟล์ไม่ได้"), "{note:?}");
        control.finish(Kind::Keys, Err("lost".into()), 4.0);
        assert_eq!(control.form.api_key, "abc");
    }

    #[test]
    fn a_409_is_said_in_plain_words() {
        let mut control = available(READY);
        control.finish(Kind::Run, reply(409, ""), 3.0);
        let note = control.note.clone().unwrap();
        assert!(!note.good);
        assert!(note.text.contains("ตอนนี้ทำไม่ได้"), "{note:?}");
        control.finish(Kind::Run, reply(409, r#"{"error":"บอทรันอยู่แล้ว"}"#), 4.0);
        assert!(control.note.unwrap().text.contains("บอทรันอยู่แล้ว"));
    }

    #[test]
    fn the_answer_to_a_button_fades_after_a_few_seconds() {
        let mut control = available(READY);
        control.finish(Kind::Stop, reply(200, "{}"), 10.0);
        assert!(screen_at(&mut control, 11.0).contains("ส่งคำสั่งหยุดแล้ว"));
        assert!(!screen_at(&mut control, 10.0 + NOTE_FOR + 1.0).contains("ส่งคำสั่งหยุดแล้ว"));
    }

    fn screen_at(control: &mut Control, now: f64) -> String {
        painted(control, now).join(" | ")
    }

    #[test]
    fn there_is_no_way_to_ask_for_real_money_on_this_panel() {
        let mut control = available(READY);
        let text = screen(&mut control).to_lowercase();
        for word in ["--live", "mainnet", "prod ", "production"] {
            assert!(!text.contains(word), "{word}: {text}");
        }
        assert!(text.contains("กระดาษ (ไม่ใช้เงิน)"));
        assert!(text.contains("testnet (เงินทดลองบนตลาดจริง)"));
    }

    #[test]
    fn the_status_chip_follows_the_job() {
        assert_eq!(
            available(READY).state(),
            (theme::State::Idle, "พร้อม".to_owned())
        );
        assert_eq!(
            available(&running("run", "testnet")).state(),
            (theme::State::Ok, "กำลังรัน · testnet".to_owned())
        );
        assert_eq!(
            available(&running("check", "testnet")).state(),
            (theme::State::Wip, "กำลังตรวจ".to_owned())
        );
    }

    // ------------------------------------- the check that also downloads data

    /// An answer with the job, output and which of the two signing keys are
    /// set spelled out.
    fn answer_with(job: Value, output: &[&str], api: bool, private: bool) -> String {
        json!({
            "setup": {"state": "ready", "detail": ""},
            "job": job,
            "output": output,
            "keys": {"api_key": api, "private_key": private, "sub_account_id": true},
            "halted": {"paper": false, "testnet": false}
        })
        .to_string()
    }

    fn job_of(action: &str, mode: &str, running: bool, code: Option<i32>) -> Value {
        json!({
            "action": action, "mode": mode, "started_at": "2026-11-17T04:00:00+00:00",
            "running": running, "exit_code": code, "stopping": false
        })
    }

    #[test]
    fn the_check_button_says_it_also_downloads_data() {
        let mut control = available(READY);
        let text = screen(&mut control);
        assert!(text.contains("ดึงข้อมูล + ตรวจกลยุทธ์"), "{text}");
        assert!(!text.contains("ตรวจกลยุทธ์ (testnet)"), "ชื่อปุ่มเก่า: {text}");
    }

    #[test]
    fn a_check_in_progress_says_it_is_fetching_candles_and_checking() {
        let mut control = available(&running("check", "testnet"));
        let text = screen(&mut control);
        assert!(text.contains("กำลังดึงแท่งเทียนและตรวจกลยุทธ์..."), "{text}");
        assert!(text.contains("เริ่ม"), "บอกเวลาเริ่มด้วย: {text}");
        // A run is still "running", not "fetching".
        let mut run = available(&running("run", "paper"));
        assert!(!screen(&mut run).contains("ดึงแท่งเทียน"));
    }

    #[test]
    fn a_check_that_never_ran_for_want_of_keys_says_to_put_them_in() {
        let none_stored = "ERROR none stored for BTC_USDT_Perp in that period. Fetch them: \
                           tradebot --config config/testnet.yaml --live --fetch-candles --days 2";
        let no_keys_line = "ยังไม่มีกุญแจ testnet · ข้ามการดึงแท่งเทียน แล้วตรวจต่อ";
        let cases = [
            // The bot's own words, though the keys are in place.
            (
                "none stored",
                answer_with(
                    job_of("check", "testnet", false, Some(1)),
                    &[none_stored],
                    true,
                    true,
                ),
            ),
            // The server's own line.
            (
                "server line",
                answer_with(
                    job_of("check", "testnet", false, Some(1)),
                    &[no_keys_line, "VERDICT  FAIL"],
                    true,
                    true,
                ),
            ),
            // The keys, as the server reports them.
            (
                "no api key",
                answer_with(
                    job_of("check", "testnet", false, Some(1)),
                    &["VERDICT  FAIL"],
                    false,
                    true,
                ),
            ),
            (
                "no private key",
                answer_with(
                    job_of("check", "testnet", false, Some(1)),
                    &["VERDICT  FAIL"],
                    true,
                    false,
                ),
            ),
            // Killed before it could say.
            (
                "no exit code",
                answer_with(
                    job_of("check", "testnet", false, None),
                    &[none_stored],
                    true,
                    true,
                ),
            ),
        ];
        for (name, body) in cases {
            let mut control = available(&body);
            let text = screen(&mut control);
            assert!(text.contains(KEYS_HINT), "{name}: {text}");
            assert!(
                !text.contains("ตรวจไม่ผ่าน"),
                "{name}: ยังไม่ได้ตรวจจริง ไม่ใช่ตรวจไม่ผ่าน: {text}"
            );
            assert_eq!(control.result().unwrap().1, theme::WARN, "{name}");
        }
    }

    #[test]
    fn the_exact_hint_the_beginner_reads() {
        assert_eq!(KEYS_HINT, "ใส่กุญแจ testnet ด้านขวาก่อน แล้วกดตรวจอีกครั้ง");
    }

    #[test]
    fn a_check_that_failed_with_the_keys_in_place_says_so_and_points_to_the_output() {
        let mut control = available(&answer_with(
            job_of("check", "testnet", false, Some(1)),
            &["VERDICT  FAIL  too few trades"],
            true,
            true,
        ));
        let text = screen(&mut control);
        assert!(text.contains("ตรวจไม่ผ่าน"), "{text}");
        assert!(text.contains("กล่องข้อความข้างล่าง"), "{text}");
        assert!(!text.contains(KEYS_HINT), "กุญแจมีแล้ว อย่าบอกให้ใส่: {text}");
        assert_eq!(control.result().unwrap().1, theme::BAD);
    }

    #[test]
    fn a_passed_check_and_other_jobs_never_ask_for_keys() {
        // A pass is a pass, whatever the key flags say.
        let mut passed = available(&answer_with(
            job_of("check", "testnet", false, Some(0)),
            &["VERDICT  PASS"],
            false,
            false,
        ));
        let text = screen(&mut passed);
        assert!(
            text.contains("ตรวจผ่าน") && !text.contains(KEYS_HINT),
            "{text}"
        );
        // A failed run is not a check.
        let mut run = available(&answer_with(
            job_of("run", "paper", false, Some(1)),
            &["none stored for x"],
            false,
            false,
        ));
        assert!(!screen(&mut run).contains(KEYS_HINT));
        // And a check still going has no verdict to explain.
        let going = available(&answer_with(
            job_of("check", "testnet", true, None),
            &[],
            false,
            false,
        ));
        assert!(going.result().is_none());
    }

    #[test]
    fn keys_are_needed_when_either_signing_key_is_missing_or_the_bot_says_no_candles() {
        let both = KeysSet {
            api_key: true,
            private_key: true,
            sub_account_id: false,
        };
        assert!(both.can_fetch(), "ไม่ต้องมี sub account ก็ดึงแท่งเทียนได้");
        assert!(!check_needs_keys(&[], both));
        assert!(check_needs_keys(&[], KeysSet::default()));
        assert!(check_needs_keys(&["None Stored For x".to_owned()], both));
        assert!(check_needs_keys(&["ยังไม่มีกุญแจ testnet".to_owned()], both));
        assert!(!check_needs_keys(&["VERDICT  FAIL".to_owned()], both));
    }

    // ------------------------------------------------------- the step guide

    fn testnet(body: &str) -> Control {
        let mut control = available(body);
        control.mode = Mode::Testnet;
        control
    }

    #[test]
    fn paper_says_it_needs_no_steps_and_no_keys() {
        let mut control = available(&answer_with(Value::Null, &[], false, false));
        let text = screen(&mut control);
        assert!(text.contains("กระดาษ: กดเริ่มได้เลย ไม่ต้องใช้กุญแจ"), "{text}");
        assert!(!text.contains("1 ใส่กุญแจ"), "{text}");
        assert!(!text.contains("3 เริ่มบอท"), "{text}");
        assert!(!text.contains("(ตอนนี้)"), "{text}");
    }

    #[test]
    fn testnet_without_keys_starts_at_step_one() {
        let mut control = testnet(&answer_with(Value::Null, &[], false, false));
        assert_eq!(control.step(), Step::Keys);
        let text = screen(&mut control);
        assert!(
            text.contains("1 ใส่กุญแจ (ตอนนี้) · 2 ตรวจกลยุทธ์ · 3 เริ่มบอท"),
            "{text}"
        );
        assert!(!text.contains("กดเริ่มได้เลย"), "{text}");
        // Half the keys is still step one.
        let half = testnet(&answer_with(Value::Null, &[], true, false));
        assert_eq!(half.step(), Step::Keys);
    }

    #[test]
    fn with_the_keys_in_the_next_step_is_the_check() {
        let mut control = testnet(&answer_with(Value::Null, &[], true, true));
        assert_eq!(control.step(), Step::Check);
        let text = screen(&mut control);
        assert!(
            text.contains("1 ใส่กุญแจ · 2 ตรวจกลยุทธ์ (ตอนนี้) · 3 เริ่มบอท"),
            "{text}"
        );
    }

    #[test]
    fn once_the_check_has_passed_the_next_step_is_to_start() {
        let mut control = testnet(&answer_with(
            job_of("check", "testnet", false, Some(0)),
            &["VERDICT  PASS"],
            true,
            true,
        ));
        assert_eq!(control.step(), Step::Start);
        let text = screen(&mut control);
        assert!(
            text.contains("1 ใส่กุญแจ · 2 ตรวจกลยุทธ์ · 3 เริ่มบอท (ตอนนี้)"),
            "{text}"
        );
    }

    #[test]
    fn a_failed_check_or_a_refused_start_sends_the_guide_back_to_the_check() {
        let mut control = testnet(&answer_with(
            job_of("check", "testnet", false, Some(0)),
            &[],
            true,
            true,
        ));
        assert_eq!(control.step(), Step::Start);
        // The strategy was edited and now fails.
        control.accept(
            reply(
                200,
                &answer_with(
                    job_of("check", "testnet", false, Some(1)),
                    &["VERDICT  FAIL"],
                    true,
                    true,
                ),
            ),
            2.0,
        );
        assert_eq!(control.step(), Step::Check);

        // Or the bot refused to start without a passed check.
        let mut refused = testnet(&answer_with(
            job_of("check", "testnet", false, Some(0)),
            &[],
            true,
            true,
        ));
        refused.accept(
            reply(
                200,
                &answer_with(
                    job_of("run", "testnet", false, Some(2)),
                    &[
                        "ERROR not sent: no passed check",
                        "ERROR run the check first",
                    ],
                    true,
                    true,
                ),
            ),
            3.0,
        );
        assert_eq!(refused.step(), Step::Check);
    }

    #[test]
    fn a_check_in_progress_is_step_two_and_a_running_testnet_bot_is_step_three() {
        let mut control = testnet(&answer_with(
            job_of("check", "testnet", false, Some(0)),
            &[],
            true,
            true,
        ));
        control.accept(
            reply(
                200,
                &answer_with(job_of("check", "testnet", true, None), &[], true, true),
            ),
            2.0,
        );
        assert_eq!(control.step(), Step::Check, "กำลังตรวจใหม่ ยังไม่ใช่ขั้นเริ่ม");

        control.accept(
            reply(
                200,
                &answer_with(
                    job_of("run", "testnet", true, None),
                    &["tick 1"],
                    true,
                    true,
                ),
            ),
            3.0,
        );
        assert_eq!(control.step(), Step::Start);
    }

    #[test]
    fn a_passed_check_is_remembered_after_other_jobs_take_its_place() {
        let mut control = testnet(&answer_with(
            job_of("check", "testnet", false, Some(0)),
            &[],
            true,
            true,
        ));
        control.accept(
            reply(
                200,
                &answer_with(job_of("resume", "testnet", false, Some(0)), &[], true, true),
            ),
            2.0,
        );
        assert_eq!(control.step(), Step::Start, "ปลดล็อกแล้วไม่ต้องตรวจใหม่");
    }

    #[test]
    fn the_guide_follows_the_mode_of_the_bot_that_is_running() {
        // Testnet is running while the radio was last left on paper.
        let mut control = available(&answer_with(
            job_of("run", "testnet", true, None),
            &[],
            true,
            true,
        ));
        control.mode = Mode::Paper;
        let text = screen(&mut control);
        assert!(text.contains("3 เริ่มบอท (ตอนนี้)"), "{text}");
    }

    #[test]
    fn the_guide_highlights_exactly_the_step_it_means() {
        let section_colour = |job: &LayoutJob, label: &str| -> Color32 {
            job.sections
                .iter()
                .find(|section| {
                    job.text[section.byte_range.start.0..section.byte_range.end.0]
                        .starts_with(label)
                })
                .unwrap_or_else(|| panic!("ไม่มีช่วงข้อความของ {label}"))
                .format
                .color
        };
        for (now, expect) in [
            (Step::Keys, [theme::ACCENT, theme::DIM, theme::DIM]),
            (Step::Check, [theme::OK, theme::ACCENT, theme::DIM]),
            (Step::Start, [theme::OK, theme::OK, theme::ACCENT]),
        ] {
            let job = guide_job(now);
            for ((_, label), colour) in STEPS.iter().zip(expect) {
                assert_eq!(section_colour(&job, label), colour, "{now:?} · {label}");
            }
            assert_eq!(
                job.text.matches("(ตอนนี้)").count(),
                1,
                "ไม่พึ่งสีอย่างเดียว: {}",
                job.text
            );
        }
    }

    #[test]
    fn the_guide_uses_no_symbol_the_fonts_lack() {
        // Arrows and ticks are not in the board's fonts: only ASCII, Thai
        // and the middle dot are allowed here.
        let mut texts = vec![PAPER_GUIDE.to_owned(), KEYS_HINT.to_owned()];
        texts.extend([Step::Keys, Step::Check, Step::Start].map(|step| guide_job(step).text));
        for text in texts {
            for c in text.chars() {
                assert!(
                    c.is_ascii() || ('\u{0E00}'..='\u{0E7F}').contains(&c) || c == '·',
                    "{c:?} ใน {text:?} ไม่มีรูปอักขระ"
                );
            }
        }
    }

    #[test]
    fn the_guide_does_not_push_the_columns_off_the_card() {
        // With the guide on top the whole panel still fits the card body.
        let mut control = testnet(&answer_with(Value::Null, &[], false, false));
        let text = screen(&mut control);
        for want in ["เริ่มบอท", "บันทึกกุญแจ", "ข้อความจากรอบล่าสุด"]
        {
            assert!(text.contains(want), "{want}: {text}");
        }
    }

    // ----------------------------------------------------- the strategy picker

    fn listing(rows: &[(&str, Option<bool>)]) -> Value {
        let list: Vec<Value> = rows
            .iter()
            .map(|(name, passed)| {
                json!({
                    "name": name,
                    "description": format!("{name} in English"),
                    "file": format!("tradebot/strategies/{name}.py"),
                    "check": passed.map(|passed| json!({"passed": passed, "why": "because"})),
                })
            })
            .collect();
        json!({ "list": list, "error": null })
    }

    /// An answer with strategies in it.
    fn with_strategies(job: Value, rows: &[(&str, Option<bool>)]) -> String {
        json!({
            "setup": {"state": "ready", "detail": ""},
            "job": job,
            "output": [],
            "keys": {"api_key": true, "private_key": true, "sub_account_id": true},
            "halted": {"paper": false, "testnet": false},
            "strategies": listing(rows),
        })
        .to_string()
    }

    const THREE: [(&str, Option<bool>); 3] = [
        ("ma_cross", Some(false)),
        ("ping_pong", Some(true)),
        ("random", Some(false)),
    ];

    #[test]
    fn the_answer_carries_the_strategies_and_the_jobs_own() {
        let none = Snapshot::parse(READY).unwrap();
        assert!(none.strategies.is_none(), "ยังไม่มีรายการ");
        let null =
            Snapshot::parse(&READY.replace("\"halted\"", "\"strategies\": null, \"halted\""));
        assert!(null.unwrap().strategies.is_none());

        let body = with_strategies(
            json!({"action": "run", "mode": "testnet", "strategy": "ma_cross", "running": true,
                   "exit_code": null, "started_at": "", "stopping": false}),
            &[("ma_cross", None)],
        );
        let snapshot = Snapshot::parse(&body).unwrap();
        assert_eq!(snapshot.job.unwrap().strategy.as_deref(), Some("ma_cross"));
        let strategies = snapshot.strategies.unwrap();
        assert_eq!(strategies.list[0].name, "ma_cross");
        assert_eq!(strategies.list[0].check_state(), CheckState::Unknown);
        assert!(strategies.error.is_none());
        // A job from before strategies existed has none.
        let old = Snapshot::parse(&running("run", "paper")).unwrap();
        assert!(old.job.unwrap().strategy.is_none());
    }

    #[test]
    fn the_rows_are_case_studies_first_the_students_own_after_and_never_random() {
        let strategies: Strategies = serde_json::from_value(listing(&[
            ("random", Some(false)),
            ("zeta", Some(true)),
            ("ma_cross", Some(false)),
            ("alpha", None),
            ("ping_pong", Some(true)),
        ]))
        .unwrap();
        let (rows, left_out) = pick_rows(&strategies);
        let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, ["ping_pong", "ma_cross", "zeta", "alpha"]);
        assert_eq!(left_out, 0);
        assert_eq!(rows[0].title, "กรณีศึกษา 1 · ping_pong");
        assert_eq!(rows[0].detail, "เครื่องทดสอบ เข้าออกถี่ให้เห็นกลไก ไม่ใช่กลยุทธ์ทำเงิน");
        assert_eq!(rows[1].title, "กรณีศึกษา 2 · ma_cross");
        assert_eq!(
            rows[1].detail,
            "ตามเทรนด์ เส้นค่าเฉลี่ยเร็วตัดเส้นช้า อาจรอหลายนาทีกว่าจะมีไม้"
        );
        assert_eq!(rows[2].title, "ของคุณ · zeta");
        assert_eq!(
            rows[2].detail,
            "zeta in English · tradebot/strategies/zeta.py"
        );
        assert_eq!(
            [rows[0].state, rows[1].state, rows[3].state],
            [
                CheckState::Passed,
                CheckState::NotPassed,
                CheckState::Unknown
            ]
        );

        // Too many of their own: the rest are counted, not drawn.
        let many: Vec<(String, Option<bool>)> =
            (0..8).map(|n| (format!("mine{n}"), Some(true))).collect();
        let many: Vec<(&str, Option<bool>)> = many.iter().map(|(n, p)| (n.as_str(), *p)).collect();
        let (rows, left_out) = pick_rows(&serde_json::from_value(listing(&many)).unwrap());
        assert_eq!((rows.len(), left_out), (MAX_OWN_SHOWN, 3));
    }

    #[test]
    fn the_three_check_states_are_worded_and_coloured_as_the_theme_says() {
        assert_eq!(CheckState::Passed.word(), ("ผ่านด่านแล้ว", theme::OK));
        assert_eq!(CheckState::NotPassed.word(), ("ยังไม่ผ่าน", theme::WARN));
        assert_eq!(CheckState::Unknown.word(), ("ไม่รู้", theme::FAINT));
        for state in [
            CheckState::Passed,
            CheckState::NotPassed,
            CheckState::Unknown,
        ] {
            assert_ne!(state.word().1, theme::ACCENT, "ส้มคือสีของระบบ ไม่ใช่คำตัดสิน");
        }
        let row = PickRow {
            name: "x".into(),
            title: "ของคุณ · x".into(),
            detail: "d".into(),
            state: CheckState::NotPassed,
            why: String::new(),
        };
        let job = pick_job(&row);
        assert_eq!(job.text, "ของคุณ · x  ·  ยังไม่ผ่าน\nd");
        let word = job
            .sections
            .iter()
            .find(|section| {
                job.text[section.byte_range.start.0..section.byte_range.end.0] == *"ยังไม่ผ่าน"
            })
            .unwrap();
        assert_eq!(word.format.color, theme::WARN);
    }

    #[test]
    fn the_default_pick_is_the_running_jobs_strategy_else_ping_pong() {
        let control = available(&with_strategies(Value::Null, &THREE));
        assert_eq!(control.selected().as_deref(), Some("ping_pong"));

        // Only the second case study listed: it is the first thing offered.
        let control = available(&with_strategies(Value::Null, &[("ma_cross", None)]));
        assert_eq!(control.selected().as_deref(), Some("ma_cross"));

        let mut job = job_of("run", "paper", true, None);
        job["strategy"] = json!("ma_cross");
        let mut control = available(&with_strategies(job, &THREE));
        control.pick = Some("ping_pong".into());
        assert_eq!(
            control.selected().as_deref(),
            Some("ma_cross"),
            "งานที่รันอยู่ชนะสิ่งที่เลือกไว้"
        );

        // Nothing listed: nothing to send, so the config decides.
        assert_eq!(available(READY).selected(), None);
    }

    #[test]
    fn a_pick_is_kept_until_it_is_no_longer_offered() {
        let mut control = available(&with_strategies(Value::Null, &THREE));
        control.pick = Some("ma_cross".into());
        assert_eq!(control.selected().as_deref(), Some("ma_cross"));
        // random is never offered, so it cannot be the pick.
        control.pick = Some("random".into());
        assert_eq!(control.selected().as_deref(), Some("ping_pong"));
        // The student deleted the file.
        control.pick = Some("ma_cross".into());
        let gone = with_strategies(Value::Null, &[("ping_pong", Some(true))]);
        control.accept(reply(200, &gone), 2.0);
        assert_eq!(control.selected().as_deref(), Some("ping_pong"));
    }

    #[test]
    fn the_strategy_goes_into_the_request_only_when_there_is_one() {
        assert_eq!(
            with_strategy(json!({"mode": "testnet"}), &Some("mine".into())),
            json!({"mode": "testnet", "strategy": "mine"})
        );
        assert_eq!(
            with_strategy(json!({}), &Some("mine".into())),
            json!({"strategy": "mine"})
        );
        assert_eq!(with_strategy(json!({}), &None), json!({}));
    }

    #[test]
    fn step_two_follows_the_strategy_picked_not_whichever_checked_last() {
        // ping_pong passed, ma_cross has not.
        let mut control = testnet(&with_strategies(Value::Null, &THREE));
        assert_eq!(control.step(), Step::Start, "ping_pong ผ่านแล้ว");
        control.pick = Some("ma_cross".into());
        assert_eq!(control.step(), Step::Check, "ma_cross ยังไม่ผ่าน");
        let text = screen(&mut control);
        assert!(text.contains("2 ตรวจกลยุทธ์ (ตอนนี้)"), "{text}");

        // Unknown is not passed.
        let control = testnet(&with_strategies(Value::Null, &[("mine", None)]));
        assert_eq!(control.step(), Step::Check);
        // Keys still come first.
        let mut nokeys = testnet(&with_strategies(Value::Null, &THREE));
        nokeys.snapshot.keys = KeysSet::default();
        assert_eq!(nokeys.step(), Step::Keys);
        // A passed check that only an old job remembers does not count once
        // the listing says otherwise.
        let mut control = testnet(&with_strategies(
            job_of("check", "testnet", false, Some(0)),
            &[("ma_cross", Some(false))],
        ));
        control.pick = Some("ma_cross".into());
        assert_eq!(control.step(), Step::Check);
    }

    #[test]
    fn the_card_lists_the_picker_with_its_words_and_hides_random() {
        let mut control = available(&with_strategies(Value::Null, &THREE));
        let text = screen(&mut control);
        for want in [
            "เลือกกลยุทธ์",
            "กรณีศึกษา 1 · ping_pong  ·  ผ่านด่านแล้ว",
            "เครื่องทดสอบ เข้าออกถี่ให้เห็นกลไก ไม่ใช่กลยุทธ์ทำเงิน",
            "กรณีศึกษา 2 · ma_cross  ·  ยังไม่ผ่าน",
            "ตามเทรนด์ เส้นค่าเฉลี่ยเร็วตัดเส้นช้า อาจรอหลายนาทีกว่าจะมีไม้",
            OWN_HINT,
        ] {
            assert!(text.contains(want), "ไม่มี {want}: {text}");
        }
        assert!(!text.contains("random"), "{text}");
        // The first case study is above the second, the hint is below both.
        let at = |needle: &str| text.find(needle).unwrap();
        assert!(at("ping_pong") < at("ma_cross") && at("ma_cross") < at(OWN_HINT));
    }

    #[test]
    fn the_students_own_strategy_shows_with_description_and_file_and_no_hint() {
        let mut control = available(&with_strategies(
            Value::Null,
            &[("ping_pong", Some(true)), ("mine", None)],
        ));
        let text = screen(&mut control);
        assert!(
            text.contains("ของคุณ · mine  ·  ไม่รู้")
                && text.contains("mine in English · tradebot/strategies/mine.py"),
            "{text}"
        );
        assert!(!text.contains("อยากได้กลยุทธ์ของตัวเอง"), "{text}");
    }

    #[test]
    fn a_listing_that_failed_says_so_and_a_missing_one_says_it_is_loading() {
        let mut control = available(READY);
        let text = screen(&mut control);
        assert!(text.contains("กำลังอ่านรายการกลยุทธ์"), "{text}");

        let body = with_strategies(Value::Null, &THREE)
            .replace("\"error\":null", "\"error\":\"boom exit code 3\"");
        let mut control = available(&body);
        let text = screen(&mut control);
        assert!(text.contains("boom exit code 3"), "{text}");
        assert!(text.contains("กรณีศึกษา 1"), "แถวล่าสุดยังอยู่: {text}");
    }

    #[test]
    fn testnet_with_an_unpassed_strategy_points_at_the_check_and_paper_does_not() {
        let mut control = testnet(&with_strategies(Value::Null, &THREE));
        control.pick = Some("ma_cross".into());
        let text = screen(&mut control);
        assert!(
            text.contains("testnet ต้องให้ ma_cross ผ่านด่านตรวจก่อน"),
            "{text}"
        );
        assert!(text.contains("กด ดึงข้อมูล + ตรวจกลยุทธ์"), "{text}");

        // A passed one: nothing to say.
        control.pick = Some("ping_pong".into());
        assert!(!screen(&mut control).contains("ต้องให้ ping_pong"));

        let mut unknown = testnet(&with_strategies(Value::Null, &[("mine", None)]));
        assert!(screen(&mut unknown).contains("ยังไม่รู้ว่า mine ผ่านด่านตรวจหรือยัง"));

        // Paper needs no check.
        let mut paper = available(&with_strategies(Value::Null, &THREE));
        paper.pick = Some("ma_cross".into());
        assert!(!screen(&mut paper).contains("ผ่านด่านตรวจก่อน"));
    }

    #[test]
    fn the_running_strategy_is_named_in_the_status_line() {
        let mut job = job_of("run", "testnet", true, None);
        job["strategy"] = json!("ma_cross");
        let mut control = available(&with_strategies(job, &THREE));
        let text = screen(&mut control);
        assert!(text.contains("กำลังรัน · testnet · ma_cross"), "{text}");
    }

    #[test]
    fn the_picker_and_all_its_notes_still_leave_room_for_the_output() {
        // The fullest the picker gets: five of the student's own, the "and
        // more" line, a failed refresh, and the check guidance under the
        // buttons - in testnet with the guide on top.
        let mut rows: Vec<(String, Option<bool>)> = vec![
            ("ping_pong".into(), Some(true)),
            ("ma_cross".into(), Some(false)),
        ];
        rows.extend((0..8).map(|n| (format!("mine{n}"), None)));
        let rows: Vec<(&str, Option<bool>)> = rows.iter().map(|(n, p)| (n.as_str(), *p)).collect();
        let body = with_strategies(Value::Null, &rows).replace(
            "\"error\":null",
            "\"error\":\"a fairly long reason the listing failed\"",
        );
        let mut control = testnet(&body);
        control.pick = Some("mine1".into());
        let found = painted_at(&mut control, 1.0);
        let heading = found
            .iter()
            .find(|(text, _)| text == "ข้อความจากรอบล่าสุด")
            .map(|(_, y)| *y)
            .expect("หัวข้อกล่องข้อความต้องยังอยู่");
        assert!(
            SLOT_BODY_HEIGHT - heading >= 90.0,
            "กล่องข้อความเหลือที่ {} px จาก {SLOT_BODY_HEIGHT}",
            SLOT_BODY_HEIGHT - heading
        );
    }
}
