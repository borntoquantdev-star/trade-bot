//! The bot's own source, for the cards that show it running.
//!
//! The five stage cards do not describe what the bot does. They show the
//! function that did it, with the lines that really ran lit up - and that
//! only works if the board can read the same files the bot imported. So this
//! hands them over, one function at a time, straight off the disk.
//!
//! Two rules, both of them about not turning into a file server:
//!
//! 1. **Only `.py` files under the code root travel.** The root is where the
//!    bot runs from, and a path carrying `..`, a drive letter or a leading
//!    slash is refused before it ever reaches the filesystem.
//! 2. **Nothing is cached.** Someone learning edits a strategy and watches
//!    the card follow; a cache would make the board lie about which code is
//!    running after exactly one edit.
//!
//! The audit trail writes file names relative to the package root
//! (`tradebot/core/engine.py`), which is the same shape this takes in, so a
//! card can ask for what the trace named without rewriting anything.

use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::Config;

/// How many lines of a whole file travel when no function was asked for.
/// A card showing a thousand lines is a card nobody reads, and the function
/// is what the question was about anyway.
const WHOLE_FILE_CAP: usize = 1200;

#[derive(Debug, Deserialize)]
pub struct Ask {
    /// `tradebot/core/engine.py`, exactly as the audit trail writes it.
    pub file: String,
    /// A line inside the function wanted. Without one the head of the file
    /// travels instead.
    pub line: Option<usize>,
}

pub async fn code(State(config): State<Config>, Query(ask): Query<Ask>) -> Json<Value> {
    Json(open(&config.code, &ask).await)
}

/// Read one file under `root` and cut the function out of it.
///
/// Separate from the handler so the refusals can be tested against a real
/// filesystem - which is the only place they matter, since the whole job of
/// `safe` is to stop a name reaching one.
pub async fn open(root: &std::path::Path, ask: &Ask) -> Value {
    let Some(relative) = safe(&ask.file) else {
        return json!({
            "present": false,
            "reason": "ชื่อไฟล์ไม่ปลอดภัยหรือไม่ใช่ไฟล์ .py",
        });
    };
    // Canonicalising both sides is what actually stops a symlink pointing
    // out of the tree; `safe` only stops the obvious spellings.
    let (Ok(base), Ok(full)) = (root.canonicalize(), root.join(&relative).canonicalize()) else {
        return json!({
            "present": false,
            "reason": format!("ไม่พบไฟล์ {}", relative),
        });
    };
    if !full.starts_with(&base) {
        return json!({
            "present": false,
            "reason": "ไฟล์อยู่นอกโฟลเดอร์โค้ด",
        });
    }
    match tokio::fs::read_to_string(&full).await {
        Ok(text) => cut(&text, &relative, ask.line),
        Err(err) => json!({
            "present": false,
            "reason": format!("อ่าน {} ไม่ได้: {err}", relative),
        }),
    }
}

/// The answer for one file, with the function picked out if one was asked for.
///
/// Pure, so the shape of the reply can be tested without a filesystem.
pub fn cut(text: &str, name: &str, line: Option<usize>) -> Value {
    let all: Vec<&str> = text.lines().collect();
    let total = all.len();
    let block = line.and_then(|line| enclosing(text, line));
    let (first, last, func) = match block {
        Some(block) => (block.first, block.last, block.name),
        None => (1, total.min(WHOLE_FILE_CAP), String::new()),
    };
    let body: Vec<&str> = all
        .iter()
        .skip(first.saturating_sub(1))
        .take(last.saturating_sub(first) + 1)
        .copied()
        .collect();
    json!({
        "present": true,
        "file": name,
        "func": func,
        "first": first,
        "last": last,
        "total": total,
        "lines": body,
    })
}

// ------------------------------------------------ the five stages, from the config

/// Which function each stage runs, worked out without the bot having run.
///
/// The trace is the truth, but it only exists after a tick. Before that the
/// cards would have nothing to show, and "nothing to show" is what a board
/// says when it is broken - so it reads the config file instead and names
/// the five functions that bot *would* run. A trace, when one arrives,
/// overrules all of it.
pub async fn stages(State(config): State<Config>) -> Json<Value> {
    let path = config.code.join(&config.config_file);
    let text = tokio::fs::read_to_string(&path).await.unwrap_or_default();
    let (exchange, strategy) = settings(&text);
    // What the bot wrote down about itself when it started. Better than the
    // config file in every case where the two disagree, because one of them
    // is a statement about a running process and the other is a file that
    // process may never have been shown.
    let running = tokio::fs::read_to_string(crate::bot::state_path(&config.logs))
        .await
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .map(|state| Running {
            strategy: module_file(text_at(&state, "strategy_module")),
            broker: module_file(text_at(&state, "broker_module")),
        })
        .unwrap_or_default();
    let root = config.code.clone();
    let mut out = Vec::new();
    for (file, func) in plan(&exchange, &strategy, &running, |name| {
        root.join(name).is_file()
    }) {
        // The line the `def` is on, so a card keys this the same way it keys
        // a function it heard about from a trace.
        let line = match tokio::fs::read_to_string(root.join(&file)).await {
            Ok(source) => find_def(&source, &func).unwrap_or(1),
            Err(_) => 1,
        };
        out.push(json!({"file": file, "func": func, "line": line}));
    }
    Json(json!({
        "present": !out.is_empty(),
        // Whether the running bot named its own modules, or this is the
        // config file being read over its shoulder. The cards say which,
        // because one of the two can be the wrong file entirely.
        "from_bot": !running.strategy.is_empty() || !running.broker.is_empty(),
        "from": config.config_file,
        "exchange": exchange,
        "strategy": strategy,
        "stages": out,
    }))
}

/// What the bot said it was running, as file names. Empty when it has not
/// run here, or when the state file predates it saying so.
#[derive(Debug, Default, Clone)]
pub struct Running {
    pub strategy: String,
    pub broker: String,
}

fn text_at<'a>(value: &'a Value, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(|found| found.as_str())
        .unwrap_or("")
}

/// `tradebot.strategies.ping_pong` -> `tradebot/strategies/ping_pong.py`.
///
/// Anything that is not a plain dotted module name comes back empty rather
/// than being turned into a path: this value reaches the filesystem, and a
/// state file is a file like any other.
pub fn module_file(module: &str) -> String {
    let ok = !module.is_empty()
        && module.starts_with("tradebot.")
        && module
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
        && !module.contains("..")
        && !module.ends_with('.');
    if !ok {
        return String::new();
    }
    format!("{}.py", module.replace('.', "/"))
}

/// `exchange` and `trading.strategy`, read out of the config file.
///
/// A line scan rather than a YAML parser: these two keys are written the
/// same way in every config this project ships, and a parser would be a
/// dependency bought to read two strings.
pub fn settings(text: &str) -> (String, String) {
    let mut exchange = String::new();
    let mut strategy = String::new();
    let mut in_trading = false;
    for line in text.lines() {
        let cut = line.split('#').next().unwrap_or("");
        if cut.trim().is_empty() {
            continue;
        }
        let top = indent(cut) == 0;
        if top {
            in_trading = cut.trim_start().starts_with("trading:");
        }
        let Some((key, value)) = cut.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().trim_matches(['"', '\'']).trim();
        if top && key == "exchange" {
            exchange = value.to_string();
        }
        if !top && in_trading && key == "strategy" {
            strategy = value.to_string();
        }
    }
    (exchange, strategy)
}

/// The five `(file, function)` pairs, given what the config says.
///
/// `exists` is handed in so the choice can be tested without a filesystem -
/// and because a strategy is registered under a name that does not always
/// match its file (`random` lives in `random_strategy.py`). The registry in
/// `tradebot/strategies/__init__.py` is the real authority; this tries the
/// two spellings that authority has ever used and falls back to the base
/// class, which is honest about being a guess.
pub fn plan(
    exchange: &str,
    strategy: &str,
    running: &Running,
    exists: impl Fn(&str) -> bool,
) -> Vec<(String, String)> {
    let broker = pick(
        &[
            running.broker.clone(),
            format!("tradebot/brokers/{exchange}.py"),
            "tradebot/brokers/base.py".to_string(),
        ],
        &exists,
    );
    let thinking = pick(
        &[
            // First the module the bot imported, which is not a guess at all.
            running.strategy.clone(),
            format!("tradebot/strategies/{strategy}.py"),
            format!("tradebot/strategies/{strategy}_strategy.py"),
            "tradebot/strategies/base.py".to_string(),
        ],
        &exists,
    );
    vec![
        (broker.clone(), "fetch_mark_price".to_string()),
        (thinking, "get_signal".to_string()),
        ("tradebot/risk.py".to_string(), "risk_gate".to_string()),
        (broker, "place_order".to_string()),
        ("tradebot/core/engine.py".to_string(), "_close".to_string()),
    ]
}

fn pick(names: &[String], exists: &impl Fn(&str) -> bool) -> String {
    names
        .iter()
        .filter(|name| !name.is_empty())
        .find(|name| exists(name))
        .cloned()
        .unwrap_or_else(|| names.last().cloned().unwrap_or_default())
}

/// The 1-based line `def <func>(` is on.
pub fn find_def(text: &str, func: &str) -> Option<usize> {
    let wanted = format!("{func}(");
    text.lines()
        .position(|line| {
            let trimmed = line.trim_start();
            trimmed
                .strip_prefix("async def ")
                .or_else(|| trimmed.strip_prefix("def "))
                .map(|rest| rest.starts_with(&wanted))
                .unwrap_or(false)
        })
        .map(|index| index + 1)
}

/// A function, where it starts and where it ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub name: String,
    /// 1-based, and counting any decorators above the `def`.
    pub first: usize,
    /// 1-based, inclusive.
    pub last: usize,
}

/// The innermost function containing `line` (1-based).
///
/// Python has no braces, so a block is found by indentation: the nearest
/// `def` above the line that is indented *less* than it, and everything
/// below that `def` indented *more*. Nested functions therefore win over the
/// one they sit in, which is the answer a person pointing at a line means.
pub fn enclosing(text: &str, line: usize) -> Option<Block> {
    let lines: Vec<&str> = text.lines().collect();
    if line == 0 || line > lines.len() {
        return None;
    }
    let target = line - 1;

    // A decorator belongs to the function under it, not to the one above.
    let mut here = target;
    if is_decorator(lines[target]) {
        while here < lines.len() && !is_def(lines[here]) {
            here += 1;
        }
        if here == lines.len() {
            return None;
        }
    }

    let start = if is_def(lines[here]) {
        here
    } else {
        let depth = depth_at(&lines, here)?;
        let mut found = None;
        for index in (0..here).rev() {
            if lines[index].trim().is_empty() {
                continue;
            }
            if is_def(lines[index]) && indent(lines[index]) < depth {
                found = Some(index);
                break;
            }
        }
        found?
    };

    let depth = indent(lines[start]);
    let mut end = start;
    // A signature spread over several lines closes its bracket somewhere
    // below the `def`, and the `) -> Decision:` that closes it sits at the
    // same indentation as the `def` itself. Reading indentation alone would
    // therefore end the function at its own signature, which is exactly what
    // happened to the most important function on the board.
    let mut open = 0i32;
    let mut signature = true;
    for (index, text) in lines.iter().enumerate().skip(start) {
        if signature {
            open += brackets(text);
            end = index;
            if open <= 0 {
                signature = false;
            }
            continue;
        }
        if text.trim().is_empty() {
            continue;
        }
        if indent(text) <= depth {
            break;
        }
        end = index;
    }

    // Decorators above the `def` are part of the function a reader sees.
    let mut first = start;
    while first > 0 && is_decorator(lines[first - 1]) {
        first -= 1;
    }

    Some(Block {
        name: name_of(lines[start]),
        first: first + 1,
        last: end + 1,
    })
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// How deep line `here` is, looking past blank lines.
///
/// A blank line has no indentation, and taking it at face value would say
/// "column zero" - so a tick that stopped on the blank line inside a
/// function would find no function at all. The nearest written line below
/// answers the question the blank one cannot; if there is none, the nearest
/// above does.
fn depth_at(lines: &[&str], here: usize) -> Option<usize> {
    if !lines.get(here)?.trim().is_empty() {
        return Some(indent(lines[here]));
    }
    lines
        .iter()
        .skip(here)
        .find(|text| !text.trim().is_empty())
        .or_else(|| {
            lines[..here]
                .iter()
                .rev()
                .find(|text| !text.trim().is_empty())
        })
        .map(|text| indent(text))
}

/// Brackets opened minus brackets closed on one line.
///
/// Only ever used on a function signature, where a bracket inside a string
/// literal would be a default value spelled to break this on purpose. The
/// cost of being wrong is a card that shows a few lines too many.
fn brackets(line: &str) -> i32 {
    line.chars().fold(0, |count, character| match character {
        '(' | '[' | '{' => count + 1,
        ')' | ']' | '}' => count - 1,
        _ => count,
    })
}

fn is_def(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("def ") || trimmed.starts_with("async def ")
}

fn is_decorator(line: &str) -> bool {
    line.trim_start().starts_with('@')
}

fn name_of(line: &str) -> String {
    let trimmed = line.trim_start();
    let after = trimmed
        .strip_prefix("async def ")
        .or_else(|| trimmed.strip_prefix("def "))
        .unwrap_or(trimmed);
    after
        .split('(')
        .next()
        .unwrap_or("")
        .trim()
        .trim_end_matches(':')
        .to_string()
}

/// The path, if it is one this is willing to read.
///
/// Refused: anything absolute, anything with a `..` step, anything with a
/// Windows drive or backslash in it, and anything that is not a `.py` file.
/// Everything else comes back with forward slashes, ready to join onto the
/// root.
pub fn safe(name: &str) -> Option<String> {
    if name.is_empty() || !name.ends_with(".py") {
        return None;
    }
    if name.contains('\\') || name.contains(':') || name.starts_with('/') {
        return None;
    }
    if name.split('/').any(|part| part == ".." || part.is_empty()) {
        return None;
    }
    Some(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"class Engine:
    def start(self) -> None:
        self.broker.connect()

    @property
    def tick(self) -> TickResult:
        now = self.clock()

        def helper():
            return 1

        return helper()

    def _record(self, result):
        pass
"#;

    #[test]
    fn a_line_in_a_method_picks_out_that_method() {
        let block = enclosing(SOURCE, 7).expect("a line inside tick");
        assert_eq!(block.name, "tick");
        assert_eq!(block.first, 5, "the decorator above it belongs to it");
        assert_eq!(block.last, 12);
    }

    #[test]
    fn the_block_stops_before_the_next_function() {
        let block = enclosing(SOURCE, 7).unwrap();
        let text: Vec<&str> = SOURCE.lines().collect();
        assert!(!text[block.last - 1].contains("_record"));
    }

    #[test]
    fn a_nested_function_wins_over_the_one_it_sits_in() {
        let block = enclosing(SOURCE, 10).expect("a line inside the helper");
        assert_eq!(block.name, "helper");
        assert_eq!(block.first, 9);
        assert_eq!(block.last, 10);
    }

    #[test]
    fn pointing_at_the_def_line_itself_picks_that_function() {
        let block = enclosing(SOURCE, 2).unwrap();
        assert_eq!(block.name, "start");
    }

    const WRAPPED: &str = r#"def risk_gate(
    signal: Signal,
    now: datetime,
) -> Decision:
    """Approve the signal, or refuse it.

    Args:
        signal: what the strategy wants.
    """
    if state.halted:
        return Reject(HALTED)
    return size(signal)


def explain(decision):
    pass
"#;

    /// The closing `) -> Decision:` of a wrapped signature sits at the same
    /// indentation as the `def`, so indentation alone ends the function at
    /// its own first line.
    #[test]
    fn a_signature_spread_over_several_lines_does_not_end_the_function() {
        let block = enclosing(WRAPPED, 10).expect("a line in the body");
        assert_eq!(block.name, "risk_gate");
        assert_eq!(block.first, 1);
        assert_eq!(block.last, 12, "down to the last line of the body");
        let lines: Vec<&str> = WRAPPED.lines().collect();
        assert!(lines[block.last - 1].contains("return size(signal)"));
    }

    /// A blank line has no indentation. Reading that as column zero means a
    /// tick that stopped on one finds no function at all - which is what the
    /// real `risk.py` did, on the blank line inside its docstring.
    #[test]
    fn a_blank_line_inside_a_function_still_finds_that_function() {
        let blank = WRAPPED.lines().nth(5).unwrap();
        assert!(blank.trim().is_empty(), "line 6 really is blank");
        let block = enclosing(WRAPPED, 6).expect("a blank line inside the docstring");
        assert_eq!(block.name, "risk_gate");
    }

    #[test]
    fn a_line_outside_any_function_has_no_block() {
        assert!(
            enclosing(SOURCE, 1).is_none(),
            "the class line is not a def"
        );
    }

    #[test]
    fn a_line_past_the_end_is_not_a_crash() {
        assert!(enclosing(SOURCE, 9999).is_none());
    }

    #[test]
    fn the_reply_carries_the_lines_and_where_they_start() {
        let value = cut(SOURCE, "tradebot/core/engine.py", Some(7));
        assert_eq!(value["func"], "tick");
        assert_eq!(value["first"], 5);
        let lines = value["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 8);
        assert!(lines[0].as_str().unwrap().contains("@property"));
    }

    #[test]
    fn without_a_line_the_head_of_the_file_travels() {
        let value = cut(SOURCE, "x.py", None);
        assert_eq!(value["first"], 1);
        assert_eq!(value["func"], "");
        assert_eq!(value["total"], SOURCE.lines().count());
    }

    #[test]
    fn a_path_climbing_out_of_the_tree_is_refused() {
        assert!(safe("../../../etc/passwd.py").is_none());
        assert!(safe("tradebot/../../secrets.py").is_none());
        assert!(safe("/etc/passwd.py").is_none());
        assert!(safe("C:/windows/system.py").is_none());
        assert!(safe("tradebot\\core\\engine.py").is_none());
    }

    #[test]
    fn only_python_files_are_served() {
        assert!(safe("config/.env").is_none());
        assert!(safe("logs/audit.jsonl").is_none());
        assert_eq!(
            safe("tradebot/core/engine.py").as_deref(),
            Some("tradebot/core/engine.py")
        );
    }

    // ------------------------------------------------- against real files

    /// Where the bot's own source is, from this crate.
    fn repo() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
    }

    fn ask(file: &str, line: Option<usize>) -> Ask {
        Ask {
            file: file.to_string(),
            line,
        }
    }

    /// The route reads the file the bot actually imported. If this breaks,
    /// the cards show somebody else's code, which is worse than showing none.
    #[tokio::test]
    async fn the_gate_comes_back_as_the_function_it_is_on_disk() {
        let value = open(&repo(), &ask("tradebot/risk.py", Some(100))).await;
        assert_eq!(value["present"], true, "{value}");
        assert_eq!(value["func"], "risk_gate");
        let lines = value["lines"].as_array().unwrap();
        let first = lines[0].as_str().unwrap();
        assert!(first.contains("def risk_gate"), "{first}");
        assert!(
            lines.len() > 40,
            "the gate is a long function; its signature alone is not it"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.as_str().unwrap_or("").contains("RejectCode.HALTED")),
            "the body travels, not just the signature"
        );
        assert!(
            value["total"].as_u64().unwrap() > 200,
            "and the whole file is longer still"
        );
    }

    #[tokio::test]
    async fn a_credential_file_next_door_is_refused_by_name_before_any_read() {
        for name in [
            "config/.env",
            "../config/.env",
            "../../../.ssh/id_rsa.py",
            "tradebot/../../.env.py",
        ] {
            let value = open(&repo(), &ask(name, None)).await;
            assert_eq!(value["present"], false, "{name} ต้องไม่ถูกเปิด");
        }
    }

    #[tokio::test]
    async fn a_file_that_is_not_there_says_so_rather_than_breaking() {
        let value = open(&repo(), &ask("tradebot/not_a_file.py", Some(3))).await;
        assert_eq!(value["present"], false);
        assert!(value["reason"].as_str().unwrap().contains("not_a_file.py"));
    }
    // ------------------------------------------- the stages from the config

    const CONFIG: &str = r#"
exchange: "grvt"           # which venue

grvt:
  env: "testnet"

trading:
  symbol: ""
  strategy: "random"         # see --list-strategies
  timeframe: "5m"
"#;

    #[test]
    fn the_two_settings_the_cards_need_are_read_out_of_the_config() {
        let (exchange, strategy) = settings(CONFIG);
        assert_eq!(exchange, "grvt");
        assert_eq!(strategy, "random", "nested under trading, not at the top");
    }

    #[test]
    fn a_comment_after_a_value_is_not_part_of_the_value() {
        let (_, strategy) = settings("trading:\n  strategy: ping_pong  # the fast one\n");
        assert_eq!(strategy, "ping_pong");
    }

    #[test]
    fn a_strategy_key_outside_trading_is_not_the_trading_strategy() {
        let (_, strategy) = settings("notes:\n  strategy: nonsense\n");
        assert_eq!(strategy, "");
    }

    #[test]
    fn the_five_stages_point_at_the_broker_and_strategy_that_are_configured() {
        let here = |name: &str| name != "tradebot/strategies/random.py";
        let plan = plan("grvt", "random", &Running::default(), here);
        assert_eq!(
            plan[0],
            ("tradebot/brokers/grvt.py".into(), "fetch_mark_price".into())
        );
        assert_eq!(
            plan[1],
            (
                "tradebot/strategies/random_strategy.py".into(),
                "get_signal".into()
            ),
            "registered as `random`, written in random_strategy.py"
        );
        assert_eq!(plan[2].1, "risk_gate");
        assert_eq!(
            plan[3],
            ("tradebot/brokers/grvt.py".into(), "place_order".into())
        );
        assert_eq!(plan[4].1, "_close");
    }

    #[test]
    fn a_strategy_nobody_can_find_falls_back_to_the_base_class() {
        let nothing = |_: &str| false;
        let plan = plan("", "homemade", &Running::default(), nothing);
        assert_eq!(plan[1].0, "tradebot/strategies/base.py");
        assert_eq!(plan[0].0, "tradebot/brokers/base.py");
    }

    #[test]
    fn a_function_is_found_by_the_line_its_def_is_on() {
        assert_eq!(find_def(SOURCE, "tick"), Some(6));
        assert_eq!(find_def(SOURCE, "helper"), Some(9));
        assert_eq!(find_def(SOURCE, "nope"), None);
    }

    #[test]
    fn a_name_that_merely_starts_the_same_is_not_the_function() {
        assert_eq!(find_def("def ticker(self):\n    pass\n", "tick"), None);
    }

    #[tokio::test]
    async fn the_planned_functions_are_really_in_the_files_they_name() {
        // The plan is only useful if every pair it names can be opened; a
        // card showing "function not found" would be worse than one showing
        // nothing.
        let root = repo();
        for (file, func) in plan("grvt", "ping_pong", &Running::default(), |name| {
            root.join(name).is_file()
        }) {
            let source = std::fs::read_to_string(root.join(&file))
                .unwrap_or_else(|_| panic!("{file} is not there"));
            assert!(find_def(&source, &func).is_some(), "{file} has no {func}()");
        }
    }

    #[test]
    fn what_the_bot_said_it_imported_beats_what_the_config_file_says() {
        // The config on disk says `random`; the bot that is running says it
        // imported ping_pong. The running bot is the one that is right.
        let running = Running {
            strategy: "tradebot/strategies/ping_pong.py".into(),
            broker: "tradebot/brokers/grvt.py".into(),
        };
        let plan = plan("paper", "random", &running, |_| true);
        assert_eq!(plan[1].0, "tradebot/strategies/ping_pong.py");
        assert_eq!(plan[0].0, "tradebot/brokers/grvt.py");
    }

    #[test]
    fn a_module_the_bot_named_but_nobody_can_find_falls_back_to_the_config() {
        let running = Running {
            strategy: "tradebot/strategies/deleted.py".into(),
            ..Default::default()
        };
        let here = |name: &str| name == "tradebot/strategies/ma_cross.py";
        assert_eq!(
            plan("", "ma_cross", &running, here)[1].0,
            "tradebot/strategies/ma_cross.py"
        );
    }

    #[test]
    fn a_module_name_becomes_the_file_it_lives_in() {
        assert_eq!(
            module_file("tradebot.strategies.ping_pong"),
            "tradebot/strategies/ping_pong.py"
        );
        assert_eq!(module_file("tradebot.risk"), "tradebot/risk.py");
    }

    #[test]
    fn a_state_file_cannot_talk_the_server_into_reading_anything_it_likes() {
        // This value arrives from a file on disk and ends up as a path, so
        // it is checked like any other name from outside.
        for nonsense in [
            "",
            "os",
            "tradebot..secrets",
            "tradebot.strategies.",
            "tradebot/strategies/x",
            "tradebot.strategies.x; rm -rf",
            "../../etc/passwd",
        ] {
            assert_eq!(module_file(nonsense), "", "{nonsense} ต้องไม่กลายเป็น path");
        }
    }

    #[tokio::test]
    async fn the_reply_says_whether_the_bot_named_its_own_modules() {
        // Not a detail: a card that shows the wrong strategy and says
        // nothing is worse than one that shows it and says where it looked.
        let value = serde_json::json!({"strategy_module": "tradebot.strategies.ping_pong"});
        assert!(!module_file(text_at(&value, "strategy_module")).is_empty());
        assert!(module_file(text_at(&value, "nothing_here")).is_empty());
    }
}
