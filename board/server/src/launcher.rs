//! What `TradeBot.exe` decides before it serves anything.
//!
//! Kept in the library, away from the console and Win32 code in the binary,
//! so the choices that matter to a student who double-clicks it - which
//! folder is the bot, where the page is, which `uv` to install with - can be
//! tested without opening a window.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::account::Account;
use crate::control::Control;
use crate::Settings;

/// The port the launcher serves on. Fixed, so the address a teacher writes
/// on the board is the address every student's browser opens.
pub const PORT: u16 = 4340;

/// Where the browser is sent.
pub fn url() -> String {
    url_for(PORT)
}

/// The same, for another port (only tests and `--port` use one).
pub fn url_for(port: u16) -> String {
    format!("http://127.0.0.1:{port}/#control")
}

/// A bot folder has `tradebot\` (the code) and `config\` (the configs the
/// buttons run). Without them nothing the page offers can work, and the
/// console window would vanish before anyone read why - so say it plainly.
pub fn check_bot_root(root: &Path) -> Result<(), String> {
    let missing: Vec<&str> = ["tradebot", "config"]
        .into_iter()
        .filter(|name| !root.join(name).is_dir())
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    Err(format!(
        "โฟลเดอร์ {} ไม่ใช่โฟลเดอร์บอท (ไม่เจอ {})\n\
         ให้วาง TradeBot.exe ไว้ในโฟลเดอร์ trade-bot ที่มี tradebot\\ และ config\\ อยู่ข้างๆ",
        root.display(),
        missing.join(" และ ")
    ))
}

/// The bot folder for an exe in `exe_dir`: that folder itself, or - for an
/// exe that was left inside `board-pack\` - the one above it.
pub fn resolve_root(exe_dir: &Path) -> PathBuf {
    if check_bot_root(exe_dir).is_ok() {
        return exe_dir.to_path_buf();
    }
    match exe_dir.parent() {
        Some(parent) if check_bot_root(parent).is_ok() => parent.to_path_buf(),
        _ => exe_dir.to_path_buf(),
    }
}

/// The page: the portable pack's, else a developer's own build.
pub fn dist_dir(root: &Path) -> PathBuf {
    let packed = root.join("board-pack").join("dist");
    if packed.join("index.html").exists() {
        return packed;
    }
    let built = root.join("board").join("app").join("dist");
    if built.join("index.html").exists() {
        return built;
    }
    // Neither: report the first, which is where a student's copy belongs.
    packed
}

/// `uv`, from the places a student's kit puts it, then from the PATH.
pub fn find_uv(root: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) { "uv.exe" } else { "uv" };
    let beside = [
        root.join("board-pack").join(name),
        root.join("tools").join(name),
    ];
    if let Some(found) = beside.into_iter().find(|path| path.is_file()) {
        return Some(found);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// The server's settings for a bot folder: every path derived from `root`,
/// testnet, control on.
pub fn settings_for(root: &Path, control: Control) -> Settings {
    let account = Account::from_files("testnet", vec![control.env_path()]);
    Settings {
        dist: dist_dir(root),
        logs: root.join("logs"),
        candles: root.join("data").join("candles"),
        code: root.to_path_buf(),
        config_file: "config/testnet.yaml".to_owned(),
        utc_offset_hours: 7,
        grvt_env: "testnet".to_owned(),
        account,
        control: Some(control),
    }
}

/// Is a board already answering on `port`? Used when the port is taken: if
/// it is our own board, the right thing is to open it, not to complain.
pub async fn board_is_running(port: u16) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_millis(1500))
        .no_proxy()
        .build()
    else {
        return false;
    };
    let Ok(reply) = client
        .get(format!("http://127.0.0.1:{port}/api/health"))
        .send()
        .await
    else {
        return false;
    };
    reply
        .json::<serde_json::Value>()
        .await
        .map(|value| value["ok"] == serde_json::Value::Bool(true))
        .unwrap_or(false)
}

/// Open the default browser. Best effort: the address is printed too, so a
/// machine where this fails is still usable.
pub fn open_browser(url: &str) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // `start` is a cmd built-in; the empty string is the window title it
        // would otherwise take the URL for.
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .creation_flags(0x0800_0000)
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let dir = std::env::temp_dir().join(format!(
                "board-launcher-{tag}-{}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn dir(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::create_dir_all(&path).unwrap();
            path
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_folder_without_the_bot_is_refused_in_words_that_name_what_is_missing() {
        let scratch = Scratch::new("root");
        let message = check_bot_root(&scratch.0).unwrap_err();
        assert!(
            message.contains("tradebot") && message.contains("config"),
            "{message}"
        );

        scratch.dir("tradebot");
        let message = check_bot_root(&scratch.0).unwrap_err();
        assert!(message.contains("config"), "{message}");
        assert!(!message.contains("ไม่เจอ tradebot"), "{message}");

        scratch.dir("config");
        assert!(check_bot_root(&scratch.0).is_ok());
    }

    #[test]
    fn an_exe_left_inside_board_pack_finds_the_bot_folder_above_it() {
        let scratch = Scratch::new("resolve");
        scratch.dir("tradebot");
        scratch.dir("config");
        let pack = scratch.dir("board-pack");
        assert_eq!(resolve_root(&pack), scratch.0);
        assert_eq!(resolve_root(&scratch.0), scratch.0);
    }

    #[test]
    fn the_packed_page_wins_over_a_developers_build() {
        let scratch = Scratch::new("dist");
        let built = scratch.dir("board").join("app").join("dist");
        std::fs::create_dir_all(&built).unwrap();
        std::fs::write(built.join("index.html"), "dev").unwrap();
        assert_eq!(dist_dir(&scratch.0), built);

        let packed = scratch.dir("board-pack").join("dist");
        std::fs::create_dir_all(&packed).unwrap();
        std::fs::write(packed.join("index.html"), "packed").unwrap();
        assert_eq!(dist_dir(&scratch.0), packed);
    }

    #[test]
    fn uv_is_looked_for_in_the_pack_before_the_tools_folder() {
        let scratch = Scratch::new("uv");
        let name = if cfg!(windows) { "uv.exe" } else { "uv" };
        let tools = scratch.dir("tools");
        std::fs::write(tools.join(name), "").unwrap();
        assert_eq!(find_uv(&scratch.0), Some(tools.join(name)));

        let pack = scratch.dir("board-pack");
        std::fs::write(pack.join(name), "").unwrap();
        assert_eq!(find_uv(&scratch.0), Some(pack.join(name)));
    }

    #[test]
    fn the_browser_is_sent_to_the_control_page_on_loopback() {
        assert_eq!(url(), "http://127.0.0.1:4340/#control");
    }
}
