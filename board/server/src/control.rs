//! Control mode: start and stop the bot from the board.
//!
//! Everything else in this server answers questions. This module is the one
//! place that *does* something, so it is built to be hard to reach and hard
//! to misuse:
//!
//! * **Not registered unless asked, and never on a public address.** See
//!   `control_enabled` in `lib.rs`. On Railway these routes do not exist.
//! * **Another web page cannot drive it.** Every request must carry a
//!   `Host` that is the loopback address (which stops DNS rebinding), every
//!   POST must carry `x-board-control: 1` (which a cross-site form cannot
//!   send), and if the browser says where the request came from (`Origin`)
//!   that must be this very server.
//! * **It runs one program: `tradebot`.** The arguments are fixed per
//!   action; nothing the browser sends is ever put on a command line.
//! * **Stopping is polite.** A stop writes `logs/stop.request`, which the bot
//!   reads and ends its run on - `--close-at-end` then flattens the position.
//!   Killing comes only after 90 seconds of silence, because a killed bot
//!   leaves a position nobody is looking after.
//! * **Keys go in, never out.** `POST /api/control/keys` merges into
//!   `config/.env`; nothing returns a value, and the output buffer is
//!   scrubbed of the two secrets in case a library ever prints one.
//!
//! The bot's own text (stdout and stderr, merged) is kept in a ring of the
//! last 300 lines so the page can show what a terminal would have shown.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::sync::Notify;

use crate::account::{self, Account};

/// Lines of output kept. The page shows the tail, as a terminal would.
pub const RING_LINES: usize = 300;

/// How long a polite stop is given before the process is killed.
pub const KILL_AFTER: Duration = Duration::from_secs(90);

/// A single line is cut here. A progress bar that never prints a newline
/// must not grow without bound.
const MAX_LINE_CHARS: usize = 2000;

/// A line of bytes is cut here before it is even decoded, for the same reason.
const MAX_LINE_BYTES: usize = 16 * 1024;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `.env` names the board and the bot agree on.
const API_KEY: &str = "GRVT_API_KEY";
const PRIVATE_KEY: &str = "GRVT_PRIVATE_KEY";
const ACCOUNT_ID: &str = "GRVT_TRADING_ACCOUNT_ID";
const SUB_ACCOUNT_ID: &str = "GRVT_SUB_ACCOUNT_ID";

/// Fewer characters than this are never scrubbed from the output: a value
/// that short would match half of every sentence.
const MIN_SECRET_CHARS: usize = 6;

// --- vocabulary ------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupState {
    Ready,
    Installing,
    Failed,
    Missing,
}

impl SetupState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Installing => "installing",
            Self::Failed => "failed",
            Self::Missing => "missing",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Run,
    Check,
    Resume,
}

impl Action {
    fn as_str(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Check => "check",
            Self::Resume => "resume",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Paper,
    Testnet,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Paper => "paper",
            Self::Testnet => "testnet",
        }
    }
}

/// The command line of each action. The only place a `tradebot` argument is
/// spelled, and the browser never contributes one.
pub fn command_args(action: Action, mode: Mode) -> Vec<String> {
    let args: &[&str] = match (action, mode) {
        (Action::Run, Mode::Paper) => &[
            "--config",
            "config/demo.yaml",
            "--dry-run",
            "--paper-live",
            "--close-at-end",
        ],
        (Action::Run, Mode::Testnet) => &[
            "--config",
            "config/testnet.yaml",
            "--live",
            "--close-at-end",
        ],
        // The check is always about the testnet file: that is the gate in
        // front of `--live`.
        (Action::Check, _) => &["--config", "config/testnet.yaml", "--check"],
        (Action::Resume, Mode::Paper) => &["--config", "config/demo.yaml", "--resume"],
        (Action::Resume, Mode::Testnet) => {
            &["--config", "config/testnet.yaml", "--live", "--resume"]
        }
    };
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

/// Download the testnet's recent 1m candles. Two days, not the check's 365:
/// the testnet keeps only about a thousand 1m bars (under a day), so asking
/// for less is not a shortcut - and the check's own default window, which
/// reaches a year back, then finds everything there is to find.
pub fn fetch_candles_args() -> Vec<String> {
    [
        "--config",
        "config/testnet.yaml",
        "--live",
        "--fetch-candles",
        "--days",
        "2",
    ]
    .map(str::to_owned)
    .to_vec()
}

/// What a check needs to do first. The check replays real candles, and a
/// student's machine has none until something downloads them; the download
/// goes through the testnet account, so it needs the same three values a
/// live run does (`require_live_credentials` in the bot).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckPlan {
    FetchThenCheck,
    CheckOnly,
}

pub fn plan_check(env_text: &str) -> CheckPlan {
    let pairs = account::parse_env(env_text);
    let filled = |name: &str| {
        pairs
            .iter()
            .any(|(key, value)| key == name && !value.trim().is_empty())
    };
    if filled(API_KEY) && filled(PRIVATE_KEY) && filled(ACCOUNT_ID) {
        CheckPlan::FetchThenCheck
    } else {
        CheckPlan::CheckOnly
    }
}

const NOTE_NO_KEYS: &str = "ยังไม่มีกุญแจ testnet จึงดึงแท่งเทียนจริงไม่ได้ \
(ต้องมี API key, private key และเลขบัญชี) · ขั้นตรวจแท่งเทียนจะขึ้นว่า \"ไม่ได้รัน\" · \
ใส่กุญแจในหน้านี้แล้วสั่งตรวจใหม่";
const NOTE_FETCH_FAILED: &str = "ดึงแท่งเทียนจริงไม่สำเร็จ (ดูข้อความด้านบน) · \
ยังรันด่านตรวจต่อ แต่ขั้นตรวจแท่งเทียนจะขึ้นว่า \"ไม่ได้รัน\"";
const NOTE_STEP_FETCH: &str = "ขั้น 1/2 · ดึงแท่งเทียนจริงจาก testnet (อ่านอย่างเดียว ไม่ส่งออเดอร์)";
const NOTE_STEP_CHECK: &str = "ขั้น 2/2 · ด่านตรวจกลยุทธ์";

/// One `tradebot` invocation of a job.
#[derive(Debug, Clone)]
struct Step {
    args: Vec<String>,
    /// Shown if this step fails and another follows it.
    failure_note: Option<String>,
}

impl Step {
    fn new(args: Vec<String>) -> Self {
        Self {
            args,
            failure_note: None,
        }
    }

    fn failing_with(mut self, note: &str) -> Self {
        self.failure_note = Some(note.to_owned());
        self
    }
}

/// A request that cannot be done, in a sentence a student can act on.
#[derive(Debug, Clone)]
pub struct Refusal {
    pub status: StatusCode,
    pub message: String,
}

impl Refusal {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({ "ok": false, "error": self.message })),
        )
            .into_response()
    }
}

// --- the output ring -------------------------------------------------------

/// The last N lines, oldest dropped first.
#[derive(Debug, Clone)]
pub struct Ring {
    lines: VecDeque<String>,
    cap: usize,
}

impl Ring {
    pub fn new(cap: usize) -> Self {
        Self {
            lines: VecDeque::with_capacity(cap.min(1024)),
            cap: cap.max(1),
        }
    }

    pub fn push(&mut self, line: String) {
        while self.lines.len() >= self.cap {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    pub fn clear(&mut self) {
        self.lines.clear();
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn to_vec(&self) -> Vec<String> {
        self.lines.iter().cloned().collect()
    }
}

// --- the controller --------------------------------------------------------

struct Job {
    id: u64,
    action: Action,
    mode: Mode,
    started_at: String,
    running: bool,
    exit_code: Option<i32>,
    stopping: bool,
    /// Wakes the watcher to kill the process. A `Notify` rather than a
    /// channel so that it can be fired from any thread, including the
    /// console's Ctrl+C handler.
    kill: Arc<Notify>,
}

struct Shared {
    setup: (SetupState, String),
    job: Option<Job>,
    output: Ring,
    /// Values scrubbed from every line before it is stored.
    secrets: Vec<String>,
}

struct Inner {
    root: PathBuf,
    program: PathBuf,
    state: Mutex<Shared>,
    /// One writer of `.env` at a time.
    env_lock: Mutex<()>,
    kill_after: Duration,
    next_id: AtomicU64,
}

/// Runs the bot for the board. Cheap to clone; all clones are one controller.
#[derive(Clone)]
pub struct Control {
    inner: Arc<Inner>,
}

impl Control {
    /// A controller for the bot folder `root`. Its program is
    /// `<root>/.venv/Scripts/tradebot.exe` (`.venv/bin/tradebot` elsewhere).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = absolute(root.into());
        let program = default_program(&root);
        Self::with(root, program, KILL_AFTER)
    }

    fn with(root: PathBuf, program: PathBuf, kill_after: Duration) -> Self {
        Self {
            inner: Arc::new(Inner {
                root,
                program,
                state: Mutex::new(Shared {
                    setup: (SetupState::Missing, MSG_MISSING.to_owned()),
                    job: None,
                    output: Ring::new(RING_LINES),
                    secrets: Vec::new(),
                }),
                env_lock: Mutex::new(()),
                kill_after,
                next_id: AtomicU64::new(1),
            }),
        }
    }

    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    /// Is the bot installed - does the program the buttons run exist?
    pub fn is_installed(&self) -> bool {
        self.inner.program.exists()
    }

    /// `<root>/config/.env`: where the keys live, for the bot and the board
    /// alike.
    pub fn env_path(&self) -> PathBuf {
        self.inner.root.join("config").join(".env")
    }

    fn logs(&self) -> PathBuf {
        self.inner.root.join("logs")
    }

    fn stop_file(&self) -> PathBuf {
        self.logs().join("stop.request")
    }

    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // -- setup --------------------------------------------------------------

    fn set_setup(&self, state: SetupState, detail: &str) {
        self.lock().setup = (state, detail.to_owned());
    }

    /// Re-check whether the bot is installed. Cheap enough to do on every
    /// poll, which is what lets a `.venv` made by hand (or by a teacher in
    /// another window) turn the page green without a restart.
    pub fn refresh_setup(&self) {
        let installed = self.inner.program.exists();
        let mut state = self.lock();
        match (state.setup.0, installed) {
            (SetupState::Installing, _) => {}
            (SetupState::Ready, false) => state.setup = (SetupState::Missing, MSG_MISSING.into()),
            (SetupState::Ready, true) => {}
            (_, true) => state.setup = (SetupState::Ready, MSG_READY.into()),
            (_, false) => {}
        }
    }

    /// Make the bot runnable: nothing to do if it is installed, otherwise
    /// build `.venv` with `uv`, streaming uv's words into the output ring.
    pub async fn run_setup(&self, uv: Option<PathBuf>) -> SetupState {
        if self.inner.program.exists() {
            self.set_setup(SetupState::Ready, MSG_READY);
            return SetupState::Ready;
        }
        let Some(uv) = uv else {
            self.set_setup(SetupState::Missing, MSG_NO_UV);
            return SetupState::Missing;
        };

        {
            let mut state = self.lock();
            state.setup = (SetupState::Installing, MSG_INSTALLING.into());
            state.output.clear();
        }

        let python = if cfg!(windows) {
            ".venv\\Scripts\\python.exe"
        } else {
            ".venv/bin/python"
        };
        let steps: [Vec<String>; 2] = [
            ["venv", ".venv", "--python", "3.11", "--allow-existing"]
                .map(str::to_owned)
                .to_vec(),
            // The grvt extra is what testnet trading imports; without it a
            // "testnet" run would stop at "the GRVT SDK is not installed".
            ["pip", "install", "--python", python, "-e", ".[grvt]"]
                .map(str::to_owned)
                .to_vec(),
        ];

        for args in steps {
            self.push_line(&format!("> uv {}", args.join(" ")));
            let mut command = child_command(&uv, &args, &self.inner.root);
            command.env("NO_COLOR", "1").env("UV_NO_PROGRESS", "1");
            let outcome = self.run_to_end(command).await;
            match outcome {
                Ok(0) => {}
                Ok(code) => {
                    self.push_line(&format!("-- uv จบด้วย exit code {code} --"));
                    self.set_setup(SetupState::Failed, MSG_FAILED);
                    return SetupState::Failed;
                }
                Err(err) => {
                    self.push_line(&format!("-- เปิด uv ไม่ได้: {err} --"));
                    self.set_setup(SetupState::Failed, MSG_FAILED);
                    return SetupState::Failed;
                }
            }
        }

        if self.inner.program.exists() {
            self.set_setup(SetupState::Ready, MSG_READY);
            SetupState::Ready
        } else {
            self.push_line("-- ติดตั้งจบแล้วแต่ไม่เจอ tradebot ใน .venv --");
            self.set_setup(SetupState::Failed, MSG_FAILED);
            SetupState::Failed
        }
    }

    /// Run a command to its end, pumping its output into the ring.
    async fn run_to_end(&self, mut command: Command) -> std::io::Result<i32> {
        let mut child = command.spawn()?;
        let out = tokio::spawn(pump(self.clone(), child.stdout.take()));
        let err = tokio::spawn(pump(self.clone(), child.stderr.take()));
        let status = child.wait().await?;
        let _ = out.await;
        let _ = err.await;
        Ok(status.code().unwrap_or(-1))
    }

    // -- output -------------------------------------------------------------

    /// Store one line, with the secrets scrubbed out of it.
    pub fn push_line(&self, line: &str) {
        let mut state = self.lock();
        let mut line = line.to_owned();
        for secret in &state.secrets {
            if line.contains(secret.as_str()) {
                line = line.replace(secret.as_str(), "***");
            }
        }
        state.output.push(line);
    }

    /// Decode a chunk the process wrote and store it as one line.
    ///
    /// Lossy on purpose: a console that is not UTF-8 must degrade a letter,
    /// not drop the line. A `\r` inside the line is a progress bar redrawing
    /// itself, so only what it ended on is kept.
    fn push_bytes(&self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        let Some(last) = text.rsplit('\r').find(|part| !part.trim().is_empty()) else {
            return;
        };
        let last = last.trim_end();
        if last.is_empty() {
            return;
        }
        let clipped: String = last.chars().take(MAX_LINE_CHARS).collect();
        self.push_line(&clipped);
    }

    fn refresh_secrets(&self) {
        let secrets = std::fs::read_to_string(self.env_path())
            .map(|text| secret_values(&text))
            .unwrap_or_default();
        self.lock().secrets = secrets;
    }

    // -- jobs ---------------------------------------------------------------

    /// Start `tradebot` for an action. One job at a time.
    pub fn start(&self, action: Action, mode: Mode) -> Result<(), Refusal> {
        let program = self.inner.program.clone();
        if action != Action::Check {
            let args = command_args(action, mode);
            return self.start_process(action, mode, program, args);
        }

        // A check is always about testnet, whatever the page said, and it is
        // the one action that can be two steps: see `plan_check`.
        let env_text = std::fs::read_to_string(self.env_path()).unwrap_or_default();
        let check = Step::new(command_args(Action::Check, Mode::Testnet));
        let (preamble, steps) = match plan_check(&env_text) {
            CheckPlan::CheckOnly => (Some(NOTE_NO_KEYS.to_owned()), vec![check]),
            CheckPlan::FetchThenCheck => (
                None,
                vec![
                    Step::new(fetch_candles_args()).failing_with(NOTE_FETCH_FAILED),
                    check,
                ],
            ),
        };
        self.start_steps(action, Mode::Testnet, program, steps, preamble)
    }

    fn start_process(
        &self,
        action: Action,
        mode: Mode,
        program: PathBuf,
        args: Vec<String>,
    ) -> Result<(), Refusal> {
        self.start_steps(action, mode, program, vec![Step::new(args)], None)
    }

    /// Start a job of one or more steps, run one after another under one job
    /// id and one output buffer. `preamble` is a line to show before the
    /// first command. The job's exit code is the last step's.
    fn start_steps(
        &self,
        action: Action,
        mode: Mode,
        program: PathBuf,
        steps: Vec<Step>,
        preamble: Option<String>,
    ) -> Result<(), Refusal> {
        let mut steps: VecDeque<Step> = steps.into();
        let Some(first) = steps.pop_front() else {
            return Err(Refusal::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "ไม่มีคำสั่งให้รัน",
            ));
        };
        let chained = !steps.is_empty();
        let args = first.args.clone();

        self.refresh_setup();
        self.refresh_secrets();

        let mut state = self.lock();
        if state.job.as_ref().is_some_and(|job| job.running) {
            return Err(Refusal::new(
                StatusCode::CONFLICT,
                "มีงานกำลังรันอยู่ · กดหยุดก่อน แล้วค่อยสั่งอันใหม่",
            ));
        }
        if state.setup.0 != SetupState::Ready {
            return Err(Refusal::new(StatusCode::CONFLICT, state.setup.1.clone()));
        }

        // A stop request left over from last time would end this run on its
        // first tick. The bot clears one too; this is the second lock on the
        // same door.
        let _ = std::fs::remove_file(self.stop_file());

        let mut command = child_command(&program, &args, &self.inner.root);
        let child = command.spawn().map_err(|err| {
            Refusal::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("เปิดบอทไม่สำเร็จ: {err}"),
            )
        })?;

        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let kill = Arc::new(Notify::new());
        state.output.clear();
        if let Some(line) = preamble {
            state.output.push(line);
        }
        if chained {
            state.output.push(NOTE_STEP_FETCH.to_owned());
        }
        state.output.push(format!("> tradebot {}", args.join(" ")));
        state.job = Some(Job {
            id,
            action,
            mode,
            started_at: now_rfc3339(),
            running: true,
            exit_code: None,
            stopping: false,
            kill: kill.clone(),
        });
        drop(state);

        let this = self.clone();
        let failure_note = first.failure_note;
        tokio::spawn(async move {
            this.run_steps(id, child, failure_note, steps, program, kill)
                .await
        });
        Ok(())
    }

    /// Own the job from its first step to its last: watch each child, and
    /// start the next step when one ends.
    ///
    /// A step that fails does not end the job if it has a `failure_note`: the
    /// note is shown and the next step still runs (the candles fetch may fail
    /// and the check must still say what it found). A stop ends the job at
    /// the step it interrupts; the steps after it never start.
    async fn run_steps(
        &self,
        id: u64,
        first: Child,
        first_note: Option<String>,
        mut rest: VecDeque<Step>,
        program: PathBuf,
        kill: Arc<Notify>,
    ) {
        let mut child = first;
        let mut note = first_note;
        loop {
            let code = self.watch(child, &kill).await;
            let Some(next) = rest.pop_front() else {
                self.finish(id, code);
                return;
            };
            if self.lock().job.as_ref().is_some_and(|job| job.stopping) {
                self.finish(id, code);
                return;
            }
            if code != 0 {
                if let Some(line) = note.take() {
                    self.push_line(&line);
                }
            }
            self.push_line(NOTE_STEP_CHECK);
            self.push_line(&format!("> tradebot {}", next.args.join(" ")));
            match child_command(&program, &next.args, &self.inner.root).spawn() {
                Ok(spawned) => child = spawned,
                Err(err) => {
                    self.push_line(&format!("-- เปิดบอทไม่สำเร็จ: {err} --"));
                    self.finish(id, -1);
                    return;
                }
            }
            note = next.failure_note;
        }
    }

    /// Own one child until it ends: collect its words, kill it if asked, and
    /// say how it finished.
    async fn watch(&self, mut child: Child, kill: &Notify) -> i32 {
        let out = tokio::spawn(pump(self.clone(), child.stdout.take()));
        let err = tokio::spawn(pump(self.clone(), child.stderr.take()));
        let (out_stop, err_stop) = (out.abort_handle(), err.abort_handle());
        let pid = child.id();

        let status = tokio::select! {
            status = child.wait() => status,
            _ = kill.notified() => {
                kill_tree(pid, &mut child).await;
                child.wait().await
            }
        };

        // The pipes close when the last holder lets go. If something the bot
        // started outlived it, they would never close - so wait a moment for
        // the tail of the output, and then stop waiting.
        let drained = tokio::time::timeout(Duration::from_secs(2), async {
            let _ = out.await;
            let _ = err.await;
        })
        .await;
        if drained.is_err() {
            out_stop.abort();
            err_stop.abort();
        }

        match status {
            Ok(status) => status.code().unwrap_or(-1),
            Err(_) => -1,
        }
    }

    fn finish(&self, id: u64, code: i32) {
        {
            let mut state = self.lock();
            if let Some(job) = state.job.as_mut().filter(|job| job.id == id) {
                job.running = false;
                job.exit_code = Some(code);
                job.stopping = false;
            }
        }
        self.push_line(&format!("-- จบการทำงาน (exit code {code}) --"));
        let _ = std::fs::remove_file(self.stop_file());
    }

    fn is_running(&self, id: u64) -> bool {
        self.lock()
            .job
            .as_ref()
            .is_some_and(|job| job.id == id && job.running)
    }

    /// Write the stop request and mark the running job as stopping.
    ///
    /// The file is written even with no job of ours running: a bot started
    /// from a terminal reads the same file, and stopping that one politely
    /// is exactly what a Stop button should do. The next start clears it.
    fn mark_stopping(&self) -> std::io::Result<Option<(u64, Action, Arc<Notify>)>> {
        std::fs::create_dir_all(self.logs())?;
        std::fs::write(self.stop_file(), "stop\n")?;
        let mut state = self.lock();
        Ok(state.job.as_mut().filter(|job| job.running).map(|job| {
            job.stopping = true;
            (job.id, job.action, job.kill.clone())
        }))
    }

    /// Ask the running job to stop. A run is given `kill_after` to end on its
    /// own (flattening its position on the way); anything else - a check, a
    /// halt reset - has no position to protect and is ended at once.
    pub fn stop(&self) -> Result<(), Refusal> {
        let marked = self.mark_stopping().map_err(|err| {
            Refusal::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("เขียนไฟล์หยุดไม่ได้: {err}"),
            )
        })?;
        let Some((id, action, kill)) = marked else {
            return Ok(());
        };
        if action != Action::Run {
            kill.notify_one();
            return Ok(());
        }
        let this = self.clone();
        let after = self.inner.kill_after;
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            if this.is_running(id) {
                this.push_line("-- ครบเวลารอแล้วบอทยังไม่หยุด · สั่งปิดโปรเซส --");
                kill.notify_one();
            }
        });
        Ok(())
    }

    /// Stop whatever is running and wait for it, for a process that is about
    /// to exit (Ctrl+C, closing the console window). Callable from any
    /// thread. Returns whether the job had ended by the time it returned.
    pub fn shutdown_blocking(&self, wait: Duration) -> bool {
        let Ok(Some((id, action, kill))) = self.mark_stopping() else {
            return true;
        };
        if action != Action::Run {
            kill.notify_one();
        }
        let deadline = Instant::now() + wait;
        while self.is_running(id) {
            if Instant::now() >= deadline {
                kill.notify_one();
                // Give the kill a moment to land before the caller exits.
                let grace = Instant::now() + Duration::from_millis(1500);
                while self.is_running(id) && Instant::now() < grace {
                    std::thread::sleep(Duration::from_millis(50));
                }
                return !self.is_running(id);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        true
    }

    // -- the page's view ----------------------------------------------------

    /// What `GET /api/control` answers.
    pub async fn snapshot(&self) -> Value {
        self.refresh_setup();
        let env_text = tokio::fs::read_to_string(self.env_path())
            .await
            .unwrap_or_default();
        let paper = read_halted(self.logs().join("state.paper.json")).await;
        let testnet = read_halted(self.logs().join("state.json")).await;

        let state = self.lock();
        json!({
            "setup": { "state": state.setup.0.as_str(), "detail": state.setup.1 },
            "job": state.job.as_ref().map(|job| json!({
                "action": job.action.as_str(),
                "mode": job.mode.as_str(),
                "started_at": job.started_at,
                "running": job.running,
                "exit_code": job.exit_code,
                "stopping": job.stopping,
            })),
            "output": state.output.to_vec(),
            "keys": key_flags(&env_text),
            "halted": { "paper": paper, "testnet": testnet },
        })
    }

    // -- keys ---------------------------------------------------------------

    /// Merge the given values into `config/.env`. Missing or empty means
    /// "leave that one alone". Returns nothing a caller could print.
    pub fn save_keys(&self, request: &KeysRequest) -> Result<(), Refusal> {
        let mut set: Vec<(&str, &str)> = Vec::new();
        if let Some(value) = &request.api_key {
            set.push((API_KEY, value));
        }
        if let Some(value) = &request.private_key {
            set.push((PRIVATE_KEY, value));
        }
        if let Some(value) = &request.sub_account_id {
            set.push((ACCOUNT_ID, value));
        }
        if set.is_empty() {
            return Ok(());
        }

        let _one_writer = self
            .inner
            .env_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let path = self.env_path();
        let existing = match std::fs::read(&path) {
            Ok(bytes) => Some(String::from_utf8(bytes).map_err(|_| {
                Refusal::new(
                    StatusCode::CONFLICT,
                    "config/.env ไม่ใช่ไฟล์ UTF-8 · เปิดในเอดิเตอร์แล้วบันทึกเป็น UTF-8 ก่อน \
                     บอร์ดจะไม่เขียนทับไฟล์ที่อ่านไม่ออก",
                )
            })?),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                return Err(Refusal::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("อ่าน config/.env ไม่ได้: {err}"),
                ))
            }
        };

        let base = existing.clone().unwrap_or_else(|| NEW_ENV.to_owned());
        // Names the config files refer to as ${NAME}. A name missing from the
        // file is an error to the bot ("environment variable is not set"),
        // an empty one is just empty - so after any write all four exist.
        let defaults = [
            (API_KEY, ""),
            (PRIVATE_KEY, ""),
            (ACCOUNT_ID, ""),
            (SUB_ACCOUNT_ID, "0"),
        ];
        let merged = merge_env(&base, &set, &defaults);

        write_atomically(&path, &merged).map_err(|err| {
            Refusal::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("เขียน config/.env ไม่สำเร็จ: {err}"),
            )
        })?;
        drop(_one_writer);
        self.refresh_secrets();
        Ok(())
    }
}

const MSG_READY: &str = "พร้อมใช้งาน";
const MSG_INSTALLING: &str = "กำลังติดตั้งตัวบอทครั้งแรก · ต้องต่ออินเทอร์เน็ต ใช้เวลาไม่กี่นาที อย่าปิดหน้าต่างสีดำ";
const MSG_FAILED: &str = "ติดตั้งไม่สำเร็จ · ดูข้อความด้านล่าง ตรวจอินเทอร์เน็ต แล้วปิดโปรแกรมเปิดใหม่เพื่อลองอีกครั้ง";
const MSG_MISSING: &str = "ยังไม่ได้ติดตั้งตัวบอท (ไม่มีโฟลเดอร์ .venv)";
const MSG_NO_UV: &str = "ยังไม่ได้ติดตั้งตัวบอท และหาโปรแกรม uv ไม่เจอ · \
ให้ครูวางไฟล์ uv.exe ไว้ในโฟลเดอร์ board-pack หรือสร้าง .venv ตามคู่มือ";

/// What a brand-new `.env` starts with.
const NEW_ENV: &str = "# กุญแจ GRVT testnet · สร้างโดยบอร์ด (TradeBot)\n\
# ไฟล์นี้อยู่ใน .gitignore · อย่าวางค่าพวกนี้ลงในแชท\n\
GRVT_ENV=testnet\n";

// --- helpers ---------------------------------------------------------------

fn absolute(path: PathBuf) -> PathBuf {
    std::path::absolute(&path).unwrap_or(path)
}

fn default_program(root: &Path) -> PathBuf {
    if cfg!(windows) {
        root.join(".venv").join("Scripts").join("tradebot.exe")
    } else {
        root.join(".venv").join("bin").join("tradebot")
    }
}

/// One child process, set up the way every one of them needs to be: run from
/// the bot folder, talking UTF-8, unbuffered (so a line appears when the bot
/// prints it, not when a buffer fills), no input, no console window.
fn child_command(program: &Path, args: &[String], cwd: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUNBUFFERED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

/// End a process and what it started.
///
/// On Windows `tradebot.exe` is a small launcher that starts `python.exe`;
/// ending only the launcher can leave the interpreter - and an open
/// position - running with no one watching it. `taskkill /T` takes the tree.
async fn kill_tree(pid: Option<u32>, child: &mut Child) {
    #[cfg(windows)]
    if let Some(pid) = pid {
        let mut taskkill = Command::new("taskkill");
        taskkill
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        taskkill.creation_flags(CREATE_NO_WINDOW);
        let _ = taskkill.status().await;
    }
    #[cfg(not(windows))]
    let _ = pid;
    let _ = child.start_kill();
}

/// Read a pipe into the ring, a line at a time.
async fn pump<R: AsyncRead + Unpin>(control: Control, reader: Option<R>) {
    let Some(mut reader) = reader else {
        return;
    };
    let mut chunk = [0u8; 4096];
    let mut pending: Vec<u8> = Vec::new();
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                for &byte in &chunk[..n] {
                    if byte == b'\n' {
                        control.push_bytes(&pending);
                        pending.clear();
                    } else {
                        pending.push(byte);
                        if pending.len() >= MAX_LINE_BYTES {
                            control.push_bytes(&pending);
                            pending.clear();
                        }
                    }
                }
            }
        }
    }
    if !pending.is_empty() {
        control.push_bytes(&pending);
    }
}

fn now_rfc3339() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0);
    chrono::DateTime::<chrono::Utc>::from_timestamp(seconds, 0)
        .map(|when| when.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

/// The `halted` flag of a bot state file. A missing, unreadable or
/// unexpected file is "not halted": the page must not cry wolf over a bot
/// that has simply never run.
pub fn halted_in(text: &str) -> bool {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|value| value.get("halted").and_then(Value::as_bool))
        .unwrap_or(false)
}

async fn read_halted(path: PathBuf) -> bool {
    tokio::fs::read_to_string(path)
        .await
        .map(|text| halted_in(&text))
        .unwrap_or(false)
}

/// Which keys are filled in - booleans, never values.
///
/// `GRVT_SUB_ACCOUNT_ID=0` does not count: that is the bot's "not set",
/// written beside a fresh key so the config's `${...}` has something to read.
pub fn key_flags(env_text: &str) -> Value {
    let pairs = account::parse_env(env_text);
    let filled = |name: &str| {
        pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.trim())
            .filter(|value| !value.is_empty())
    };
    json!({
        "api_key": filled(API_KEY).is_some(),
        "private_key": filled(PRIVATE_KEY).is_some(),
        "sub_account_id": filled(ACCOUNT_ID).is_some()
            || filled(SUB_ACCOUNT_ID).is_some_and(|value| value != "0"),
    })
}

/// The two values that must never appear in the output ring.
fn secret_values(env_text: &str) -> Vec<String> {
    let pairs = account::parse_env(env_text);
    [API_KEY, PRIVATE_KEY]
        .iter()
        .filter_map(|name| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.trim().to_owned())
        })
        .filter(|value| value.chars().count() >= MIN_SECRET_CHARS)
        .collect()
}

/// Is this `.env` line an assignment to `name`? Returns the text before the
/// `=` with any `export ` removed, the way both readers of this file see it.
fn assigned_name(line: &str) -> Option<&str> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
    line.split_once('=').map(|(name, _)| name.trim())
}

/// Put `set` into `.env` text, and make sure every name in `defaults` exists.
///
/// Everything else is left exactly as it was: comments, blank lines, other
/// variables, their order. A name already present has its **first** line
/// replaced (the bot and the server both take the first one); a name that is
/// not is appended. Line endings follow the file, so a Notepad-saved CRLF
/// file does not come back with mixed endings.
pub fn merge_env(text: &str, set: &[(&str, &str)], defaults: &[(&str, &str)]) -> String {
    // A byte-order mark in front of the first name makes the bot read that
    // name with the mark attached - i.e. as a different variable. Rewriting
    // the file is the chance to drop it.
    let text = text.trim_start_matches('\u{feff}');
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };

    let mut lines: Vec<String> = if text.is_empty() {
        Vec::new()
    } else {
        let mut lines: Vec<String> = text
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
            .collect();
        if text.ends_with('\n') {
            lines.pop();
        }
        lines
    };

    for (name, value) in set {
        let found = lines
            .iter()
            .position(|line| assigned_name(line) == Some(*name));
        match found {
            Some(index) => {
                let export = if lines[index].trim_start().starts_with("export ") {
                    "export "
                } else {
                    ""
                };
                lines[index] = format!("{export}{name}={value}");
            }
            None => lines.push(format!("{name}={value}")),
        }
    }
    for (name, value) in defaults {
        if !lines.iter().any(|line| assigned_name(line) == Some(*name)) {
            lines.push(format!("{name}={value}"));
        }
    }

    let mut merged = lines.join(eol);
    merged.push_str(eol);
    merged
}

/// Write beside the file and rename over it, so a crash or a full disk never
/// leaves half a key behind. If the rename is refused (something has the
/// file open) fall back to a plain write.
fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("env.tmp");
    std::fs::write(&temp, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600));
    }
    if std::fs::rename(&temp, path).is_err() {
        let _ = std::fs::remove_file(&temp);
        std::fs::write(path, text)?;
    }
    Ok(())
}

// --- keys: what the page may send ------------------------------------------

/// Validated, trimmed values. Deliberately not `Debug`: nothing here should
/// ever be formatted into a log line by accident.
#[derive(Default)]
pub struct KeysRequest {
    pub api_key: Option<String>,
    pub private_key: Option<String>,
    pub sub_account_id: Option<String>,
}

fn clean_value(label: &str, raw: &str) -> Result<Option<String>, Refusal> {
    let value = raw.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let bad = |why: &str| Refusal::new(StatusCode::BAD_REQUEST, format!("{label} {why}"));
    if value.chars().count() > 2048 {
        return Err(bad("ยาวผิดปกติ"));
    }
    if value.starts_with('=') {
        return Err(bad("ขึ้นต้นด้วย = ไม่ได้"));
    }
    // One whitespace or control character anywhere would split a line in
    // `.env` or hide inside it. Real keys have neither.
    if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(bad("ต้องไม่มีช่องว่างหรือขึ้นบรรทัดใหม่ · ลองคัดลอกใหม่ให้ครบ"));
    }
    Ok(Some(value.to_owned()))
}

fn field(body: &Value, name: &str, label: &str) -> Result<Option<String>, Refusal> {
    match body.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => clean_value(label, text),
        // An account id is a number, and a page may well send it as one.
        Some(Value::Number(number))
            if name == "sub_account_id" && (number.is_u64() || number.is_i64()) =>
        {
            clean_value(label, &number.to_string())
        }
        Some(_) => Err(Refusal::new(
            StatusCode::BAD_REQUEST,
            format!("{label} ต้องเป็นข้อความ"),
        )),
    }
}

pub fn parse_keys(body: &[u8]) -> Result<KeysRequest, Refusal> {
    // The error is a fixed sentence: serde's own text can quote the input.
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| Refusal::new(StatusCode::BAD_REQUEST, "อ่านข้อมูลที่ส่งมาไม่ออก"))?;
    if !value.is_object() {
        return Err(Refusal::new(
            StatusCode::BAD_REQUEST,
            "ข้อมูลที่ส่งมาต้องเป็น JSON object",
        ));
    }
    Ok(KeysRequest {
        api_key: field(&value, "api_key", "API key")?,
        private_key: field(&value, "private_key", "private key")?,
        sub_account_id: field(&value, "sub_account_id", "sub account id")?,
    })
}

fn parse_mode(body: &[u8]) -> Result<Mode, Refusal> {
    let value: Option<Value> = serde_json::from_slice(body).ok();
    match value
        .as_ref()
        .and_then(|value| value.get("mode"))
        .and_then(Value::as_str)
    {
        Some("paper") => Ok(Mode::Paper),
        Some("testnet") => Ok(Mode::Testnet),
        _ => Err(Refusal::new(
            StatusCode::BAD_REQUEST,
            "ต้องระบุ mode เป็น paper หรือ testnet",
        )),
    }
}

// --- the door ---------------------------------------------------------------

/// Is `host` (a `Host` header: name, optional port) the loopback address?
fn is_loopback_host(host: &str) -> bool {
    let name = if let Some(rest) = host.strip_prefix('[') {
        match rest.split_once(']') {
            Some((inside, _)) => format!("[{inside}]"),
            None => return false,
        }
    } else {
        match host.rsplit_once(':') {
            Some((name, port)) if port.chars().all(|c| c.is_ascii_digit()) => name.to_owned(),
            _ => host.to_owned(),
        }
    };
    matches!(
        name.to_ascii_lowercase().as_str(),
        "127.0.0.1" | "localhost" | "[::1]"
    )
}

/// The checks that keep other web pages from driving this one. `Err` carries
/// the reason, for the 403 body.
pub fn check_request(method: &Method, headers: &HeaderMap, uri: &Uri) -> Result<(), &'static str> {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .or_else(|| {
            uri.authority()
                .map(|authority| authority.as_str().to_owned())
        })
        .ok_or("ไม่มี Host")?;

    // A page on evil.example can make its own name point at 127.0.0.1 and
    // then talk to this server as "same origin". Its Host header still says
    // evil.example, which is what gives it away.
    if !is_loopback_host(&host) {
        return Err("ใช้ได้เฉพาะจาก 127.0.0.1 หรือ localhost");
    }

    if let Some(origin) = headers.get(header::ORIGIN) {
        let origin = origin.to_str().map_err(|_| "Origin อ่านไม่ออก")?;
        // `null` (a sandboxed frame, a file:// page) has no `://` and so
        // never equals a host.
        let origin_host = origin.split_once("://").map_or(origin, |(_, rest)| rest);
        if !origin_host.eq_ignore_ascii_case(&host) {
            return Err("คำสั่งมาจากเว็บอื่น ไม่ใช่หน้าบอร์ดนี้");
        }
    }

    if method != Method::GET && method != Method::HEAD {
        let marked = headers
            .get("x-board-control")
            .and_then(|value| value.to_str().ok())
            == Some("1");
        if !marked {
            return Err("ขาด header x-board-control: 1");
        }
    }
    Ok(())
}

async fn guard(request: Request, next: Next) -> Response {
    let verdict = check_request(request.method(), request.headers(), request.uri());
    let mut response = match verdict {
        Ok(()) => next.run(request).await,
        Err(why) => Refusal::new(StatusCode::FORBIDDEN, why).into_response(),
    };
    // Whether a key is set and what the bot just printed are not things for
    // an HTTP cache to keep.
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

// --- routes ------------------------------------------------------------------

#[derive(Clone)]
struct Ctx {
    control: Control,
    account: Account,
}

/// The control routes, already behind their guard. `lib.rs` merges this in
/// only when control is enabled; with it off none of these paths exist.
pub fn routes(control: Control, account: Account) -> Router {
    Router::new()
        .route("/api/control", get(status))
        .route("/api/control/run", post(run))
        .route("/api/control/stop", post(stop))
        .route("/api/control/check", post(check))
        .route("/api/control/resume", post(resume))
        .route("/api/control/keys", post(keys))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn(guard))
        .with_state(Ctx { control, account })
}

/// Success answers with the same document `GET /api/control` gives, plus
/// `"ok": true`, so a page can redraw from the reply without asking again.
async fn done(ctx: &Ctx) -> Response {
    let mut snapshot = ctx.control.snapshot().await;
    if let Some(object) = snapshot.as_object_mut() {
        object.insert("ok".into(), Value::Bool(true));
    }
    Json(snapshot).into_response()
}

async fn status(State(ctx): State<Ctx>) -> Json<Value> {
    Json(ctx.control.snapshot().await)
}

async fn run(State(ctx): State<Ctx>, body: Bytes) -> Response {
    let mode = match parse_mode(&body) {
        Ok(mode) => mode,
        Err(refusal) => return refusal.into_response(),
    };
    match ctx.control.start(Action::Run, mode) {
        Ok(()) => done(&ctx).await,
        Err(refusal) => refusal.into_response(),
    }
}

async fn resume(State(ctx): State<Ctx>, body: Bytes) -> Response {
    let mode = match parse_mode(&body) {
        Ok(mode) => mode,
        Err(refusal) => return refusal.into_response(),
    };
    match ctx.control.start(Action::Resume, mode) {
        Ok(()) => done(&ctx).await,
        Err(refusal) => refusal.into_response(),
    }
}

async fn check(State(ctx): State<Ctx>) -> Response {
    match ctx.control.start(Action::Check, Mode::Testnet) {
        Ok(()) => done(&ctx).await,
        Err(refusal) => refusal.into_response(),
    }
}

async fn stop(State(ctx): State<Ctx>) -> Response {
    match ctx.control.stop() {
        Ok(()) => done(&ctx).await,
        Err(refusal) => refusal.into_response(),
    }
}

async fn keys(State(ctx): State<Ctx>, body: Bytes) -> Response {
    let request = match parse_keys(&body) {
        Ok(request) => request,
        Err(refusal) => return refusal.into_response(),
    };
    let control = ctx.control.clone();
    let saved = tokio::task::spawn_blocking(move || control.save_keys(&request)).await;
    match saved {
        Ok(Ok(())) => {}
        Ok(Err(refusal)) => return refusal.into_response(),
        Err(_) => {
            return Refusal::new(StatusCode::INTERNAL_SERVER_ERROR, "บันทึกกุญแจไม่สำเร็จ")
                .into_response()
        }
    }
    // The board's own account cards pick the new key up now, not at the next
    // start.
    ctx.account.reload().await;
    done(&ctx).await
}

#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;
