//! `TradeBot.exe` - double-click, and the bot is yours to drive from a page.
//!
//! What a student sees: a console window with a few Thai lines, and the
//! browser opening on the board's control page. What happens underneath:
//!
//! 1. The folder this exe sits in is the bot folder (`--root` overrides it
//!    for development). If it has no `tradebot\` and `config\` the window
//!    says so and waits for Enter, because a console that closes by itself
//!    takes its own error message with it.
//! 2. The board's server starts on `127.0.0.1:4340` with control mode on.
//!    If a board is already there, the browser is simply opened on it.
//! 3. In the background, if there is no `.venv` yet, `uv` builds one - the
//!    one-time install the student never has to type.
//!
//! # Not leaving a bot behind
//!
//! A bot started from the page must not outlive the window that started it,
//! and must not be killed in a way that strands a position. Two layers:
//!
//! * **Polite first.** Ctrl+C or closing the window writes the bot's stop
//!   file and waits for it to flatten and exit (20 s for Ctrl+C; about 3 s
//!   for a closing window, which is all Windows allows a handler).
//! * **Guaranteed second.** At start-up this process joins a Windows *job
//!   object* flagged `KILL_ON_JOB_CLOSE`. Every child (the bot, `uv`,
//!   `taskkill`) is born inside it, so whichever way this process ends -
//!   the handler above, Task Manager, a crash - the kernel ends them too.
//!   It is joined by this process rather than assigned to each child so
//!   there is no child that can be missed, and it is joined *after* the
//!   browser is opened so that closing this window does not close the
//!   student's browser with it.

#![allow(non_snake_case)]

use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use board_server::control::{Control, SetupState};
use board_server::{launcher, serve_on};

fn main() -> ExitCode {
    install_panic_hook();
    match run() {
        Ok(code) => code,
        Err(message) => {
            say("");
            say(&message);
            wait_for_enter();
            ExitCode::FAILURE
        }
    }
}

struct Args {
    root: Option<PathBuf>,
    port: u16,
    open_browser: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        root: None,
        port: launcher::PORT,
        open_browser: true,
    };
    let mut rest = std::env::args().skip(1);
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--root" => {
                let value = rest.next().ok_or("--root ต้องตามด้วยพาธโฟลเดอร์")?;
                args.root = Some(PathBuf::from(value));
            }
            // For testing the launcher without fighting a real board over
            // the port or opening a browser each time.
            "--port" => {
                let value = rest.next().ok_or("--port ต้องตามด้วยตัวเลข")?;
                args.port = value.parse().map_err(|_| "--port ต้องเป็นตัวเลข")?;
            }
            "--no-browser" => args.open_browser = false,
            other => return Err(format!("ไม่รู้จักตัวเลือก {other}")),
        }
    }
    Ok(args)
}

fn run() -> Result<ExitCode, String> {
    prepare_console();
    let args = parse_args()?;

    let root = match args.root {
        Some(root) => std::path::absolute(&root).unwrap_or(root),
        None => {
            let exe = std::env::current_exe()
                .map_err(|err| format!("หาตำแหน่งของ TradeBot.exe ไม่เจอ: {err}"))?;
            let dir = exe
                .parent()
                .ok_or("หาโฟลเดอร์ของ TradeBot.exe ไม่เจอ")?
                .to_path_buf();
            launcher::resolve_root(&dir)
        }
    };

    say("TradeBot - ตัวควบคุมบอทเทรด (testnet = เงินทดลอง ไม่ใช่เงินจริง)");
    launcher::check_bot_root(&root)?;
    say(&format!("โฟลเดอร์บอท: {}", root.display()));

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("เริ่มระบบไม่ได้: {err}"))?;
    runtime.block_on(serve(root, args.port, args.open_browser))
}

async fn serve(root: PathBuf, port: u16, open_browser: bool) -> Result<ExitCode, String> {
    let url = launcher::url_for(port);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(err) => {
            if launcher::board_is_running(port).await {
                say("บอร์ดเปิดอยู่แล้ว - เปิดหน้าเว็บให้อีกครั้ง");
                say(&format!("   {url}"));
                if open_browser {
                    launcher::open_browser(&url);
                }
                return Ok(ExitCode::SUCCESS);
            }
            return Err(format!(
                "เปิดพอร์ต {port} ไม่ได้ ({err})\n\
                 มีโปรแกรมอื่นใช้พอร์ตนี้อยู่ ลองปิดโปรแกรมนั้น หรือรีสตาร์ทเครื่องแล้วเปิดใหม่"
            ));
        }
    };

    let control = Control::new(&root);
    let settings = launcher::settings_for(&root, control.clone());
    let index = settings.dist.join("index.html");
    if !index.exists() {
        return Err(format!(
            "ไม่เจอหน้าเว็บของบอร์ด ({})\n\
             ต้องมีโฟลเดอร์ board-pack\\dist ในโฟลเดอร์บอท - แตก zip ของชุดนักเรียนให้ครบ",
            index.display()
        ));
    }

    let server = tokio::spawn(serve_on(listener, settings, std::future::pending::<()>()));

    say(&format!("บอร์ดเปิดที่ {url}"));
    if open_browser {
        launcher::open_browser(&url);
    }
    say("ปิดหน้าต่างนี้ = ปิดโปรแกรม (บอทที่กำลังรันจะถูกสั่งหยุดอย่างสุภาพก่อน)");
    say("");

    // After the browser: see the module notes on the job object.
    keep_children_with_us(&control);

    let setup = control.clone();
    let uv = launcher::find_uv(&root);
    tokio::spawn(async move {
        let installed = setup.is_installed();
        if !installed && uv.is_some() {
            say("กำลังติดตั้งตัวบอทครั้งแรก (ต้องต่ออินเทอร์เน็ต ใช้เวลาไม่กี่นาที) ...");
        }
        match setup.run_setup(uv).await {
            SetupState::Ready if installed => say("ตัวบอทพร้อมใช้งาน"),
            SetupState::Ready => say("ติดตั้งเสร็จแล้ว - ตัวบอทพร้อมใช้งาน"),
            SetupState::Failed => {
                say("ติดตั้งไม่สำเร็จ - ดูรายละเอียดในหน้าเว็บ แล้วปิดโปรแกรมเปิดใหม่เพื่อลองอีกครั้ง")
            }
            SetupState::Missing => {
                say("ยังไม่ได้ติดตั้งตัวบอท และหา uv ไม่เจอ - ให้ครูวาง uv.exe ไว้ใน board-pack")
            }
            SetupState::Installing => {}
        }
    });

    match server.await {
        Ok(Ok(())) => Ok(ExitCode::SUCCESS),
        Ok(Err(err)) => Err(format!("เซิร์ฟเวอร์ของบอร์ดหยุดทำงาน: {err}")),
        Err(err) => Err(format!("เซิร์ฟเวอร์ของบอร์ดหยุดทำงาน: {err}")),
    }
}

/// Print a line. Never panics: a console that has been closed under us must
/// not turn a polite shutdown into a crash.
fn say(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

fn wait_for_enter() {
    say("");
    say("กด Enter เพื่อปิดหน้าต่างนี้ ...");
    let mut ignored = String::new();
    let _ = std::io::stdin().read_line(&mut ignored);
}

fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        say("");
        say(&format!("โปรแกรมพังกลางคัน: {info}"));
        wait_for_enter();
    }));
}

// --- Windows: console, job object, Ctrl+C -----------------------------------

#[cfg(windows)]
fn prepare_console() {
    win::prepare_console();
}

#[cfg(not(windows))]
fn prepare_console() {}

#[cfg(windows)]
fn keep_children_with_us(control: &Control) {
    if let Err(why) = win::join_kill_on_close_job() {
        // Not fatal: the polite stop below still works. But say so, because
        // without the job object a bot could outlive a crash of this window.
        say(&format!("คำเตือน: ผูกบอทกับหน้าต่างนี้ไม่สำเร็จ ({why})"));
    }
    win::on_console_event(control.clone());
}

#[cfg(not(windows))]
fn keep_children_with_us(control: &Control) {
    let control = control.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        say("กำลังสั่งบอทให้หยุดอย่างสุภาพ แล้วปิดโปรแกรม ...");
        let _ = tokio::task::spawn_blocking(move || {
            control.shutdown_blocking(std::time::Duration::from_secs(20))
        })
        .await;
        std::process::exit(0);
    });
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::sync::OnceLock;
    use std::time::Duration;

    use board_server::control::Control;
    use windows_sys::core::BOOL;
    use windows_sys::Win32::Foundation::FALSE;
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleCtrlHandler, SetConsoleMode, SetConsoleOutputCP,
        SetConsoleTitleW, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT, CTRL_LOGOFF_EVENT,
        CTRL_SHUTDOWN_EVENT, ENABLE_EXTENDED_FLAGS, ENABLE_QUICK_EDIT_MODE, STD_INPUT_HANDLE,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    use super::say;

    /// UTF-8 on the way out, a title, and no QuickEdit.
    ///
    /// QuickEdit is the console's click-to-select: one stray click in the
    /// window freezes every program that writes to it until a key is
    /// pressed. A student who clicks the black window to "see if it is
    /// working" would pause the launcher.
    pub fn prepare_console() {
        // SAFETY: plain Win32 calls with valid arguments; failures are
        // harmless (a launcher with no console, e.g. under a test harness).
        unsafe {
            SetConsoleOutputCP(65001);
            let title: Vec<u16> = "TradeBot".encode_utf16().chain(Some(0)).collect();
            SetConsoleTitleW(title.as_ptr());
            let input = GetStdHandle(STD_INPUT_HANDLE);
            let mut mode = 0u32;
            if GetConsoleMode(input, &mut mode) != 0 {
                SetConsoleMode(
                    input,
                    (mode | ENABLE_EXTENDED_FLAGS) & !ENABLE_QUICK_EDIT_MODE,
                );
            }
        }
    }

    /// Put this process in a job whose handle, held for the life of the
    /// process, kills everything in it when it closes. Children inherit
    /// membership, so the bot and `uv` are covered with no bookkeeping.
    pub fn join_kill_on_close_job() -> Result<(), String> {
        // SAFETY: the struct is zero-initialised as the API documents, the
        // size passed is its size, and the handle is deliberately leaked.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err("CreateJobObject".into());
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(info).cast::<c_void>(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if set == 0 {
                return Err("SetInformationJobObject".into());
            }
            if AssignProcessToJobObject(job, GetCurrentProcess()) == 0 {
                return Err("AssignProcessToJobObject".into());
            }
        }
        Ok(())
    }

    static CONTROL: OnceLock<Control> = OnceLock::new();

    /// Runs on a thread Windows creates for the event, so blocking here is
    /// fine - it is the only place that can wait for the bot to finish.
    unsafe extern "system" fn handler(kind: u32) -> BOOL {
        let wait = match kind {
            // The user is at the keyboard and can wait for a flatten.
            CTRL_C_EVENT | CTRL_BREAK_EVENT => Duration::from_secs(20),
            // Windows ends the process about 5 s after a close event no
            // matter what; the job object covers whatever is left.
            CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => Duration::from_secs(3),
            _ => return FALSE,
        };
        if let Some(control) = CONTROL.get() {
            say("กำลังสั่งบอทให้หยุดอย่างสุภาพ แล้วปิดโปรแกรม ...");
            if !control.shutdown_blocking(wait) {
                say("บอทยังไม่หยุดทันเวลา - ปิดโปรเซสของบอทแล้ว");
            }
        }
        std::process::exit(0)
    }

    pub fn on_console_event(control: Control) {
        let _ = CONTROL.set(control);
        // SAFETY: `handler` has the signature the API wants and lives for
        // the whole process.
        unsafe {
            SetConsoleCtrlHandler(Some(handler), 1);
        }
    }
}
