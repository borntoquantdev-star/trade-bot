//! Tests for `control.rs`, kept in their own file because the module they
//! cover is long enough already. Included from there as `mod tests`.

use super::*;
use axum::body::Body;
use axum::http::{HeaderName, Request as HttpRequest};
use tower::ServiceExt;

/// A folder under the system's temp dir that removes itself.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "board-control-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn env(&self) -> PathBuf {
        self.0.join("config").join(".env")
    }

    fn stop_file(&self) -> PathBuf {
        self.0.join("logs").join("stop.request")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A program that exists and ends at once, printing `hello`.
fn quick() -> (PathBuf, Vec<String>) {
    if cfg!(windows) {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        (
            PathBuf::from(root).join("System32").join("cmd.exe"),
            ["/C", "echo", "hello"].map(str::to_owned).to_vec(),
        )
    } else {
        (PathBuf::from("/bin/echo"), vec!["hello".to_owned()])
    }
}

/// A program that exists and keeps going for about half a minute.
fn slow() -> (PathBuf, Vec<String>) {
    if cfg!(windows) {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        (
            PathBuf::from(root).join("System32").join("ping.exe"),
            ["-n", "30", "127.0.0.1"].map(str::to_owned).to_vec(),
        )
    } else {
        (PathBuf::from("/bin/sleep"), vec!["30".to_owned()])
    }
}

fn controller(scratch: &Scratch, program: PathBuf, kill_after: Duration) -> Control {
    Control::with(scratch.0.clone(), program, kill_after)
}

fn request(method: &str, uri: &str, headers: &[(&str, &str)], body: &str) -> HttpRequest<Body> {
    let mut built = HttpRequest::builder()
        .method(method)
        .uri(uri)
        .body(Body::from(body.to_owned()))
        .unwrap();
    let mut all = vec![("host", "127.0.0.1:4340")];
    all.extend_from_slice(headers);
    for (name, value) in all {
        // `insert`, so a test's own `host` replaces the default one.
        built.headers_mut().insert(
            HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_str(value).unwrap(),
        );
    }
    built
}

/// A POST the way the board's own page sends it.
fn post(uri: &str, body: &str) -> HttpRequest<Body> {
    request("POST", uri, &[("x-board-control", "1")], body)
}

async fn send(app: &Router, request: HttpRequest<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn wait_until_ended(control: &Control) -> Value {
    for _ in 0..200 {
        let snapshot = control.snapshot().await;
        if snapshot["job"]["running"] == Value::Bool(false) {
            return snapshot;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the job did not end in 10 s");
}

// -- the ring ------------------------------------------------------------

#[test]
fn the_ring_keeps_only_the_most_recent_lines() {
    let mut ring = Ring::new(RING_LINES);
    for n in 0..1000 {
        ring.push(format!("line {n}"));
    }
    assert_eq!(ring.len(), 300);
    let lines = ring.to_vec();
    assert_eq!(lines.first().unwrap(), "line 700");
    assert_eq!(lines.last().unwrap(), "line 999");
    ring.clear();
    assert!(ring.is_empty());
}

#[test]
fn a_progress_bar_redrawing_itself_leaves_one_line_not_forty() {
    let scratch = Scratch::new("cr");
    let control = controller(&scratch, quick().0, KILL_AFTER);
    control.push_bytes(b"10%\r50%\r100% done\r\n");
    control.push_bytes(b"\r\n");
    control.push_bytes(b"   \r");
    control.push_bytes("ข้อความไทย".as_bytes());
    control.push_bytes(&[0xff, b'o', b'k']);
    let lines = control.lock().output.to_vec();
    assert_eq!(lines[0], "100% done");
    assert_eq!(lines[1], "ข้อความไทย");
    assert!(lines[2].ends_with("ok"), "{lines:?}");
    assert_eq!(lines.len(), 3, "blank lines are not kept: {lines:?}");
}

#[test]
fn a_huge_line_is_cut_rather_than_kept_whole() {
    let scratch = Scratch::new("long");
    let control = controller(&scratch, quick().0, KILL_AFTER);
    control.push_bytes("ก".repeat(10_000).as_bytes());
    let lines = control.lock().output.to_vec();
    assert_eq!(lines[0].chars().count(), MAX_LINE_CHARS);
}

#[test]
fn the_two_secrets_are_scrubbed_from_whatever_the_bot_prints() {
    let scratch = Scratch::new("scrub");
    std::fs::create_dir_all(scratch.env().parent().unwrap()).unwrap();
    std::fs::write(
        scratch.env(),
        "GRVT_API_KEY=apikey-abcdef-123\nGRVT_PRIVATE_KEY=0xprivate-fedcba-987\nGRVT_TRADING_ACCOUNT_ID=42\n",
    )
    .unwrap();
    let control = controller(&scratch, quick().0, KILL_AFTER);
    control.refresh_secrets();
    control.push_line("login with apikey-abcdef-123 and 0xprivate-fedcba-987 ok");
    let lines = control.lock().output.to_vec();
    assert_eq!(lines[0], "login with *** and *** ok");
}

// -- what each button runs -------------------------------------------------

#[test]
fn each_action_runs_exactly_the_command_the_contract_names() {
    let run = |action, mode| command_args(action, mode).join(" ");
    assert_eq!(
        run(Action::Run, Mode::Paper),
        "--config config/demo.yaml --dry-run --paper-live --close-at-end"
    );
    assert_eq!(
        run(Action::Run, Mode::Testnet),
        "--config config/testnet.yaml --live --close-at-end"
    );
    assert_eq!(
        run(Action::Check, Mode::Testnet),
        "--config config/testnet.yaml --check"
    );
    assert_eq!(
        run(Action::Resume, Mode::Paper),
        "--config config/demo.yaml --resume"
    );
    assert_eq!(
        run(Action::Resume, Mode::Testnet),
        "--config config/testnet.yaml --live --resume"
    );
}

// -- halted and keys, read from files --------------------------------------

#[test]
fn halted_is_read_from_the_state_file_and_a_missing_file_is_not_halted() {
    assert!(halted_in(r#"{"halted": true, "day": "2026-11-17"}"#));
    assert!(!halted_in(r#"{"halted": false}"#));
    assert!(!halted_in(r#"{"day": "2026-11-17"}"#));
    assert!(
        !halted_in(r#"{"halted": "yes"}"#),
        "only a real boolean counts"
    );
    assert!(!halted_in("not json at all"));
    assert!(!halted_in(""));
}

#[tokio::test]
async fn halted_comes_from_state_paper_json_and_state_json_separately() {
    let scratch = Scratch::new("halted");
    let control = controller(&scratch, quick().0, KILL_AFTER);
    let snapshot = control.snapshot().await;
    assert_eq!(
        snapshot["halted"],
        json!({ "paper": false, "testnet": false })
    );

    let logs = scratch.0.join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(logs.join("state.paper.json"), r#"{"halted": true}"#).unwrap();
    let snapshot = control.snapshot().await;
    assert_eq!(
        snapshot["halted"],
        json!({ "paper": true, "testnet": false })
    );

    std::fs::write(logs.join("state.json"), r#"{"halted": true}"#).unwrap();
    let snapshot = control.snapshot().await;
    assert_eq!(
        snapshot["halted"],
        json!({ "paper": true, "testnet": true })
    );
}

#[test]
fn a_key_counts_as_set_only_when_it_has_a_value() {
    let flags = key_flags("GRVT_API_KEY=abc\nGRVT_PRIVATE_KEY=\nGRVT_SUB_ACCOUNT_ID=0\n");
    assert_eq!(
        flags,
        json!({ "api_key": true, "private_key": false, "sub_account_id": false })
    );
    let flags = key_flags("GRVT_TRADING_ACCOUNT_ID=5421993573613261\n");
    assert_eq!(flags["sub_account_id"], json!(true));
    let flags = key_flags("GRVT_SUB_ACCOUNT_ID=77\n");
    assert_eq!(
        flags["sub_account_id"],
        json!(true),
        "the alternate name counts"
    );
    assert_eq!(
        key_flags(""),
        json!({ "api_key": false, "private_key": false, "sub_account_id": false })
    );
}

// -- merging into .env ---------------------------------------------------------

#[test]
fn merging_keeps_every_other_line_comment_and_the_order() {
    let before = "# my notes\nGRVT_ENV=testnet\n\nGRVT_API_KEY=old\n# keep me\nOTHER=1\n";
    let after = merge_env(before, &[("GRVT_API_KEY", "new")], &[]);
    assert_eq!(
        after,
        "# my notes\nGRVT_ENV=testnet\n\nGRVT_API_KEY=new\n# keep me\nOTHER=1\n"
    );
}

#[test]
fn a_missing_name_is_appended_and_the_first_duplicate_is_the_one_replaced() {
    let after = merge_env(
        "A=1\nGRVT_API_KEY=first\nGRVT_API_KEY=second\n",
        &[("GRVT_API_KEY", "new")],
        &[],
    );
    assert_eq!(after, "A=1\nGRVT_API_KEY=new\nGRVT_API_KEY=second\n");

    let after = merge_env("A=1", &[("GRVT_PRIVATE_KEY", "p")], &[]);
    assert_eq!(
        after, "A=1\nGRVT_PRIVATE_KEY=p\n",
        "a file with no final newline still ends well"
    );
}

#[test]
fn a_commented_out_key_is_not_mistaken_for_the_key() {
    let after = merge_env("# GRVT_API_KEY=old\n", &[("GRVT_API_KEY", "new")], &[]);
    assert_eq!(after, "# GRVT_API_KEY=old\nGRVT_API_KEY=new\n");
}

#[test]
fn windows_line_endings_stay_windows_line_endings() {
    let after = merge_env(
        "A=1\r\nGRVT_API_KEY=old\r\n",
        &[("GRVT_API_KEY", "new")],
        &[("GRVT_PRIVATE_KEY", "")],
    );
    assert_eq!(after, "A=1\r\nGRVT_API_KEY=new\r\nGRVT_PRIVATE_KEY=\r\n");
    assert!(
        !after.replace("\r\n", "").contains('\n'),
        "no bare newline crept in"
    );
}

#[test]
fn an_export_prefix_is_kept_and_a_byte_order_mark_is_dropped() {
    let after = merge_env(
        "\u{feff}export GRVT_API_KEY=old\n",
        &[("GRVT_API_KEY", "new")],
        &[],
    );
    assert_eq!(after, "export GRVT_API_KEY=new\n");
}

#[test]
fn defaults_fill_only_the_names_that_are_missing() {
    let defaults = [
        (API_KEY, ""),
        (PRIVATE_KEY, ""),
        (ACCOUNT_ID, ""),
        (SUB_ACCOUNT_ID, "0"),
    ];
    let after = merge_env(
        "GRVT_API_KEY=k\nGRVT_SUB_ACCOUNT_ID=77\n",
        &[("GRVT_PRIVATE_KEY", "p")],
        &defaults,
    );
    assert_eq!(
        after,
        "GRVT_API_KEY=k\nGRVT_SUB_ACCOUNT_ID=77\nGRVT_PRIVATE_KEY=p\nGRVT_TRADING_ACCOUNT_ID=\n"
    );
}

// -- the keys route ----------------------------------------------------------------

#[tokio::test]
async fn saving_a_key_creates_the_file_with_testnet_and_never_echoes_a_value() {
    let scratch = Scratch::new("keys");
    let control = controller(&scratch, quick().0, KILL_AFTER);
    let app = routes(control, Account::default());

    let body = json!({
        "api_key": "  api-SECRET-1111  ",
        "private_key": "0xPRIVATE-SECRET-2222",
        "sub_account_id": 5421993573613261u64,
    })
    .to_string();
    let (status, reply) = send(&app, post("/api/control/keys", &body)).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(
        reply["keys"],
        json!({ "api_key": true, "private_key": true, "sub_account_id": true })
    );

    let written = std::fs::read_to_string(scratch.env()).unwrap();
    assert!(written.contains("GRVT_ENV=testnet"), "{written}");
    assert!(
        written.contains("GRVT_API_KEY=api-SECRET-1111\n"),
        "trimmed: {written}"
    );
    assert!(written.contains("GRVT_PRIVATE_KEY=0xPRIVATE-SECRET-2222"));
    assert!(written.contains("GRVT_TRADING_ACCOUNT_ID=5421993573613261"));
    assert!(
        written.contains("GRVT_SUB_ACCOUNT_ID=0"),
        "the testnet config reads GRVT_SUB_ACCOUNT_ID too, so the name must exist: {written}"
    );

    // Nothing the page can read - the reply, or a later poll - holds a value.
    let (_, later) = send(&app, request("GET", "/api/control", &[], "")).await;
    for text in [reply.to_string(), later.to_string()] {
        for secret in [
            "api-SECRET-1111",
            "0xPRIVATE-SECRET-2222",
            "5421993573613261",
        ] {
            assert!(!text.contains(secret), "{secret} leaked into {text}");
        }
    }
}

#[tokio::test]
async fn saving_one_key_leaves_the_rest_of_the_file_and_the_other_keys_alone() {
    let scratch = Scratch::new("merge");
    std::fs::create_dir_all(scratch.env().parent().unwrap()).unwrap();
    std::fs::write(
        scratch.env(),
        "# กุญแจของฉัน\nGRVT_API_KEY=keep-this-one\nGRVT_PRIVATE_KEY=old-private\n\
         GRVT_TRADING_ACCOUNT_ID=111\nGRVT_SUB_ACCOUNT_ID=0\nMY_NOTE=hello\n",
    )
    .unwrap();
    let control = controller(&scratch, quick().0, KILL_AFTER);
    let app = routes(control, Account::default());

    // Empty and missing both mean "unchanged".
    let (status, _) = send(
        &app,
        post(
            "/api/control/keys",
            r#"{"private_key": "new-private", "api_key": "", "sub_account_id": null}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        std::fs::read_to_string(scratch.env()).unwrap(),
        "# กุญแจของฉัน\nGRVT_API_KEY=keep-this-one\nGRVT_PRIVATE_KEY=new-private\n\
         GRVT_TRADING_ACCOUNT_ID=111\nGRVT_SUB_ACCOUNT_ID=0\nMY_NOTE=hello\n"
    );
}

#[tokio::test]
async fn a_value_that_could_break_the_file_is_refused_and_the_file_is_not_touched() {
    let scratch = Scratch::new("bad");
    let control = controller(&scratch, quick().0, KILL_AFTER);
    let app = routes(control, Account::default());
    for bad in [
        "abcQ\nGRVT_ENV=prod",
        "abcQ\r\nX=1",
        "=abcQ",
        "ab cd",
        "ab\tcd",
        "ab\u{0}cd",
    ] {
        let body = json!({ "api_key": bad }).to_string();
        let (status, reply) = send(&app, post("/api/control/keys", &body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?} -> {reply}");
        assert!(
            !reply.to_string().contains("abcQ"),
            "the reply must not quote the value"
        );
    }
    let (status, _) = send(&app, post("/api/control/keys", r#"{"api_key": ["x"]}"#)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = send(&app, post("/api/control/keys", "not json")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        !scratch.env().exists(),
        "nothing valid was sent, so nothing was written"
    );
}

#[tokio::test]
async fn an_env_file_that_is_not_utf8_is_left_exactly_as_it_was() {
    let scratch = Scratch::new("ansi");
    std::fs::create_dir_all(scratch.env().parent().unwrap()).unwrap();
    let original: Vec<u8> = vec![b'#', b' ', 0xa1, 0xd8, 0xad, b'\n', b'A', b'=', b'1', b'\n'];
    std::fs::write(scratch.env(), &original).unwrap();
    let control = controller(&scratch, quick().0, KILL_AFTER);
    let app = routes(control, Account::default());
    let (status, _) = send(&app, post("/api/control/keys", r#"{"api_key": "abcdef"}"#)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(std::fs::read(scratch.env()).unwrap(), original);
}

// -- the door ------------------------------------------------------------------------

#[tokio::test]
async fn a_post_without_the_header_is_refused_with_403() {
    let scratch = Scratch::new("nohdr");
    let control = controller(&scratch, quick().0, KILL_AFTER);
    let app = routes(control, Account::default());
    for uri in [
        "/api/control/run",
        "/api/control/stop",
        "/api/control/check",
        "/api/control/resume",
        "/api/control/keys",
    ] {
        let (status, _) = send(&app, request("POST", uri, &[], r#"{"mode":"paper"}"#)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
    }
    // A header that is present but is not exactly "1" does not count.
    let (status, _) = send(
        &app,
        request(
            "POST",
            "/api/control/stop",
            &[("x-board-control", "true")],
            "",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        !scratch.stop_file().exists(),
        "a refused stop must not have written the stop file"
    );
}

#[tokio::test]
async fn a_request_from_another_web_page_is_refused_even_with_the_header() {
    let scratch = Scratch::new("origin");
    let control = controller(&scratch, quick().0, KILL_AFTER);
    let app = routes(control, Account::default());
    for origin in [
        "http://evil.example",
        "http://127.0.0.1:9999",
        "http://localhost:4340",
        "null",
        "https://127.0.0.1:4340x",
    ] {
        let (status, _) = send(
            &app,
            request(
                "POST",
                "/api/control/stop",
                &[("x-board-control", "1"), ("origin", origin)],
                "",
            ),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "Origin {origin}");
    }
    // The board's own page: Origin equals Host.
    let (status, _) = send(
        &app,
        request(
            "POST",
            "/api/control/stop",
            &[
                ("x-board-control", "1"),
                ("origin", "http://127.0.0.1:4340"),
            ],
            "",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_host_that_is_not_loopback_is_refused_which_is_what_stops_dns_rebinding() {
    let scratch = Scratch::new("host");
    let control = controller(&scratch, quick().0, KILL_AFTER);
    let app = routes(control, Account::default());
    // Origin equals Host here, so only the Host check can catch it.
    let (status, _) = send(
        &app,
        request(
            "POST",
            "/api/control/stop",
            &[
                ("host", "evil.example:4340"),
                ("origin", "http://evil.example:4340"),
                ("x-board-control", "1"),
            ],
            "",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = send(
        &app,
        request("GET", "/api/control", &[("host", "192.168.1.20:4340")], ""),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a GET is not exempt");

    for host in [
        "127.0.0.1:4340",
        "localhost:4340",
        "[::1]:4340",
        "LOCALHOST",
    ] {
        let (status, _) = send(&app, request("GET", "/api/control", &[("host", host)], "")).await;
        assert_eq!(status, StatusCode::OK, "{host}");
    }
}

#[test]
fn host_names_are_judged_by_their_name_not_their_port_or_a_lookalike() {
    assert!(is_loopback_host("127.0.0.1"));
    assert!(is_loopback_host("127.0.0.1:4340"));
    assert!(is_loopback_host("localhost:4340"));
    assert!(is_loopback_host("[::1]:4340"));
    assert!(!is_loopback_host("127.0.0.1.evil.example:4340"));
    assert!(!is_loopback_host("evil.example"));
    assert!(!is_loopback_host("localhost.evil.example"));
    assert!(!is_loopback_host("0.0.0.0:4340"));
    assert!(!is_loopback_host("[::1"));
}

#[tokio::test]
async fn every_control_reply_says_not_to_cache_it() {
    let scratch = Scratch::new("cache");
    let control = controller(&scratch, quick().0, KILL_AFTER);
    let app = routes(control, Account::default());
    let response = app
        .clone()
        .oneshot(request("GET", "/api/control", &[], ""))
        .await
        .unwrap();
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let response = app
        .oneshot(request("POST", "/api/control/stop", &[], ""))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
}

// -- jobs --------------------------------------------------------------------------------

#[tokio::test]
async fn nothing_can_start_until_the_bot_is_installed() {
    let scratch = Scratch::new("missing");
    let control = Control::new(&scratch.0);
    let snapshot = control.snapshot().await;
    assert_eq!(snapshot["setup"]["state"], "missing");
    assert_eq!(snapshot["job"], Value::Null);

    let app = routes(control, Account::default());
    let (status, reply) = send(&app, post("/api/control/run", r#"{"mode":"paper"}"#)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(reply["ok"], json!(false));
    assert!(
        reply["error"].as_str().unwrap().contains(".venv"),
        "{reply}"
    );
}

#[tokio::test]
async fn a_mode_other_than_paper_or_testnet_is_a_400() {
    let scratch = Scratch::new("mode");
    let control = controller(&scratch, quick().0, KILL_AFTER);
    let app = routes(control, Account::default());
    for body in ["", "{}", r#"{"mode":"prod"}"#, r#"{"mode":1}"#, "nope"] {
        let (status, _) = send(&app, post("/api/control/run", body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body:?}");
        let (status, _) = send(&app, post("/api/control/resume", body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body:?}");
    }
}

#[tokio::test]
async fn a_job_runs_to_its_end_and_its_words_and_exit_code_are_kept() {
    let scratch = Scratch::new("quick");
    let (program, args) = quick();
    let control = controller(&scratch, program.clone(), KILL_AFTER);
    control
        .start_process(Action::Run, Mode::Paper, program, args)
        .unwrap();

    let snapshot = wait_until_ended(&control).await;
    assert_eq!(snapshot["setup"]["state"], "ready");
    let job = &snapshot["job"];
    assert_eq!(job["action"], "run");
    assert_eq!(job["mode"], "paper");
    assert_eq!(job["exit_code"], json!(0));
    assert_eq!(job["stopping"], json!(false));
    let started = job["started_at"].as_str().unwrap();
    assert!(
        started.ends_with('Z') && started.len() == 20,
        "RFC 3339: {started}"
    );
    let output: Vec<&str> = snapshot["output"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        output.iter().any(|line| line.trim() == "hello"),
        "{output:?}"
    );
    assert!(output.last().unwrap().contains("exit code 0"), "{output:?}");
}

#[tokio::test]
async fn a_second_job_is_refused_while_one_runs_and_a_stop_ends_it() {
    let scratch = Scratch::new("slow");
    let (program, args) = slow();
    // A short kill timer: this is the "still alive after the wait" path.
    let control = controller(&scratch, program.clone(), Duration::from_millis(400));
    control
        .start_process(Action::Run, Mode::Testnet, program.clone(), args.clone())
        .unwrap();

    let second = control.start_process(Action::Run, Mode::Paper, program, args);
    assert_eq!(second.unwrap_err().status, StatusCode::CONFLICT);

    let app = routes(control.clone(), Account::default());
    let (status, reply) = send(&app, post("/api/control/stop", "")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply["job"]["stopping"], json!(true));
    assert_eq!(
        reply["job"]["running"],
        json!(true),
        "a stop is polite: it does not kill at once"
    );
    assert!(
        scratch.stop_file().exists(),
        "the stop file is how the bot is asked to finish"
    );

    let snapshot = wait_until_ended(&control).await;
    assert_ne!(snapshot["job"]["exit_code"], Value::Null);
    assert_eq!(snapshot["job"]["stopping"], json!(false));
    assert!(
        !scratch.stop_file().exists(),
        "a finished job leaves no stale stop request behind"
    );
    assert!(
        snapshot["output"].to_string().contains("ครบเวลารอ"),
        "the page should be told why it was killed"
    );
}

#[tokio::test]
async fn a_run_is_given_its_time_before_it_is_killed() {
    let scratch = Scratch::new("polite");
    let (program, args) = slow();
    let control = controller(&scratch, program.clone(), Duration::from_secs(600));
    control
        .start_process(Action::Run, Mode::Paper, program, args)
        .unwrap();
    control.stop().unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await;
    let snapshot = control.snapshot().await;
    assert_eq!(
        snapshot["job"]["running"],
        json!(true),
        "still within its 90 s"
    );
    assert_eq!(snapshot["job"]["stopping"], json!(true));

    // The path a closing launcher takes: wait a little, then end it rather
    // than orphan it.
    let for_thread = control.clone();
    let ended = tokio::task::spawn_blocking(move || {
        for_thread.shutdown_blocking(Duration::from_millis(300))
    })
    .await
    .unwrap();
    assert!(ended);
    wait_until_ended(&control).await;
}

#[tokio::test]
async fn a_check_has_no_position_to_protect_so_a_stop_ends_it_at_once() {
    let scratch = Scratch::new("check");
    let (program, args) = slow();
    let control = controller(&scratch, program.clone(), Duration::from_secs(600));
    control
        .start_process(Action::Check, Mode::Testnet, program, args)
        .unwrap();
    control.stop().unwrap();
    let snapshot = wait_until_ended(&control).await;
    assert_eq!(snapshot["job"]["action"], "check");
    assert_eq!(snapshot["job"]["mode"], "testnet");
}

#[tokio::test]
async fn a_stop_with_nothing_running_still_writes_the_file_for_a_bot_started_elsewhere() {
    let scratch = Scratch::new("idle");
    let control = controller(&scratch, quick().0, KILL_AFTER);
    let app = routes(control, Account::default());
    let (status, reply) = send(&app, post("/api/control/stop", "")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply["job"], Value::Null);
    assert!(scratch.stop_file().exists());
}

#[tokio::test]
async fn starting_a_run_clears_a_stop_request_left_over_from_before() {
    let scratch = Scratch::new("stale");
    std::fs::create_dir_all(scratch.0.join("logs")).unwrap();
    std::fs::write(scratch.stop_file(), "stop\n").unwrap();
    let (program, args) = quick();
    let control = controller(&scratch, program.clone(), KILL_AFTER);
    control
        .start_process(Action::Run, Mode::Paper, program, args)
        .unwrap();
    wait_until_ended(&control).await;
    assert!(!scratch.stop_file().exists());
}

// -- setup -------------------------------------------------------------------------------

#[tokio::test]
async fn setup_is_ready_when_installed_and_missing_without_uv() {
    let scratch = Scratch::new("setup");
    let control = Control::new(&scratch.0);
    assert_eq!(control.run_setup(None).await, SetupState::Missing);
    let snapshot = control.snapshot().await;
    assert_eq!(snapshot["setup"]["state"], "missing");
    assert!(snapshot["setup"]["detail"].as_str().unwrap().contains("uv"));

    let installed = controller(&scratch, quick().0, KILL_AFTER);
    assert_eq!(installed.run_setup(None).await, SetupState::Ready);
    assert_eq!(installed.snapshot().await["setup"]["state"], "ready");
}

#[tokio::test]
async fn a_uv_that_cannot_start_is_a_failed_setup_with_a_reason() {
    let scratch = Scratch::new("baduv");
    let control = Control::new(&scratch.0);
    let state = control
        .run_setup(Some(scratch.0.join("no-such-uv.exe")))
        .await;
    assert_eq!(state, SetupState::Failed);
    let snapshot = control.snapshot().await;
    assert_eq!(snapshot["setup"]["state"], "failed");
    assert!(!snapshot["output"].as_array().unwrap().is_empty());
}

// -- a check that fetches candles first ----------------------------------------------

const ALL_THREE: &str =
    "GRVT_API_KEY=a-key-123\nGRVT_PRIVATE_KEY=0xprivate-456\nGRVT_TRADING_ACCOUNT_ID=789\n";

#[test]
fn a_check_fetches_candles_first_only_when_the_account_keys_are_all_there() {
    assert_eq!(plan_check(ALL_THREE), CheckPlan::FetchThenCheck);
    // The `.env` the board writes, with everything filled in.
    assert_eq!(
        plan_check(&format!(
            "GRVT_ENV=testnet\n{ALL_THREE}GRVT_SUB_ACCOUNT_ID=0\n"
        )),
        CheckPlan::FetchThenCheck
    );

    // No file at all, empty values, a commented-out key, and a missing one.
    assert_eq!(plan_check(""), CheckPlan::CheckOnly);
    assert_eq!(
        plan_check("GRVT_API_KEY=\nGRVT_PRIVATE_KEY=\nGRVT_TRADING_ACCOUNT_ID=\n"),
        CheckPlan::CheckOnly
    );
    assert_eq!(
        plan_check("# GRVT_API_KEY=a\nGRVT_PRIVATE_KEY=b\nGRVT_TRADING_ACCOUNT_ID=1\n"),
        CheckPlan::CheckOnly
    );
    assert_eq!(
        plan_check("GRVT_API_KEY=a-key-123\nGRVT_PRIVATE_KEY=0xprivate-456\n"),
        CheckPlan::CheckOnly,
        "the download needs the account id too: it is a live-credentials call"
    );
    assert_eq!(
        plan_check("GRVT_API_KEY=a-key-123\nGRVT_TRADING_ACCOUNT_ID=789\n"),
        CheckPlan::CheckOnly
    );
}

#[test]
fn the_fetch_is_two_days_of_testnet_candles_and_the_check_is_unchanged() {
    assert_eq!(
        fetch_candles_args().join(" "),
        "--config config/testnet.yaml --live --fetch-candles --days 2"
    );
    assert_eq!(
        command_args(Action::Check, Mode::Testnet).join(" "),
        "--config config/testnet.yaml --check"
    );
}

/// The command line of a shell that runs `command` and exits with its code.
fn sh(command: &str) -> (PathBuf, Vec<String>) {
    if cfg!(windows) {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        (
            PathBuf::from(root).join("System32").join("cmd.exe"),
            vec!["/C".to_owned(), command.to_owned()],
        )
    } else {
        (
            PathBuf::from("/bin/sh"),
            vec!["-c".to_owned(), command.to_owned()],
        )
    }
}

fn step(command: &str) -> Step {
    Step::new(sh(command).1)
}

fn lines_of(snapshot: &Value) -> Vec<String> {
    snapshot["output"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|line| line.as_str().map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn the_jobs_exit_code_is_the_last_steps_whatever_the_first_one_did() {
    // The fetch fails, the check passes: the job passed, and says why the
    // fetch did not matter to it.
    let scratch = Scratch::new("chain-a");
    let program = sh("exit 0").0;
    let control = controller(&scratch, program.clone(), KILL_AFTER);
    control
        .start_steps(
            Action::Check,
            Mode::Testnet,
            program.clone(),
            vec![
                step("exit 7").failing_with("fetch did not work"),
                step("echo second step ran"),
            ],
            None,
        )
        .unwrap();
    let snapshot = wait_until_ended(&control).await;
    assert_eq!(snapshot["job"]["action"], "check");
    assert_eq!(
        snapshot["job"]["exit_code"],
        json!(0),
        "the check's code, not the fetch's"
    );
    let output = lines_of(&snapshot);
    let at = |needle: &str| output.iter().position(|line| line.contains(needle));
    let (note, second) = (at("fetch did not work"), at("second step ran"));
    assert!(note.is_some() && second.is_some(), "{output:?}");
    assert!(
        note < second,
        "the note comes before the next step: {output:?}"
    );
    assert!(at("ขั้น 2/2").is_some(), "{output:?}");

    // The fetch passes, the check fails: the job failed with the check's code.
    let scratch = Scratch::new("chain-b");
    let control = controller(&scratch, program.clone(), KILL_AFTER);
    control
        .start_steps(
            Action::Check,
            Mode::Testnet,
            program,
            vec![
                step("echo first step ran").failing_with("must not be shown"),
                step("exit 5"),
            ],
            None,
        )
        .unwrap();
    let snapshot = wait_until_ended(&control).await;
    assert_eq!(snapshot["job"]["exit_code"], json!(5));
    let output = lines_of(&snapshot);
    assert!(
        !output.iter().any(|line| line.contains("must not be shown")),
        "a step that passed has no failure to explain: {output:?}"
    );
    assert!(output.last().unwrap().contains("exit code 5"), "{output:?}");
}

#[tokio::test]
async fn a_stop_during_the_first_step_ends_the_job_and_the_check_never_starts() {
    let scratch = Scratch::new("chain-stop");
    let (program, slow_args) = slow();
    let control = controller(&scratch, program.clone(), Duration::from_secs(600));
    control
        .start_steps(
            Action::Check,
            Mode::Testnet,
            program,
            vec![Step::new(slow_args.clone()), Step::new(slow_args)],
            None,
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    // A check holds no position, so a stop kills the step at once.
    control.stop().unwrap();
    let snapshot = wait_until_ended(&control).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let snapshot_later = control.snapshot().await;
    assert_ne!(snapshot["job"]["exit_code"], Value::Null);
    assert_eq!(
        snapshot_later["job"]["running"],
        json!(false),
        "no second step was started"
    );
    assert!(
        !lines_of(&snapshot_later)
            .iter()
            .any(|line| line.contains("ขั้น 2/2")),
        "{:?}",
        lines_of(&snapshot_later)
    );
}

/// `ping`/`sleep` refuse the bot's options and exit at once, which is all
/// these two tests need from a stand-in for `tradebot`.
#[tokio::test]
async fn a_check_without_keys_says_so_and_runs_only_the_check() {
    let scratch = Scratch::new("check-nokeys");
    let control = controller(&scratch, slow().0, KILL_AFTER);
    control.start(Action::Check, Mode::Paper).unwrap();
    let snapshot = wait_until_ended(&control).await;
    assert_eq!(snapshot["job"]["action"], "check");
    assert_eq!(snapshot["job"]["mode"], "testnet");
    let output = lines_of(&snapshot);
    assert!(
        output[0].starts_with("ยังไม่มีกุญแจ testnet จึงดึงแท่งเทียนจริงไม่ได้"),
        "{output:?}"
    );
    assert_eq!(output[1], "> tradebot --config config/testnet.yaml --check");
    assert!(
        !output.iter().any(|line| line.contains("--fetch-candles")),
        "no keys: nothing is fetched: {output:?}"
    );
}

#[tokio::test]
async fn a_check_with_keys_fetches_two_days_then_runs_the_check_in_one_job() {
    let scratch = Scratch::new("check-keys");
    std::fs::create_dir_all(scratch.env().parent().unwrap()).unwrap();
    std::fs::write(scratch.env(), ALL_THREE).unwrap();
    let control = controller(&scratch, slow().0, KILL_AFTER);
    control.start(Action::Check, Mode::Testnet).unwrap();
    let snapshot = wait_until_ended(&control).await;
    assert_eq!(snapshot["job"]["action"], "check");
    let output = lines_of(&snapshot);
    let at = |needle: &str| output.iter().position(|line| line.contains(needle));
    let fetch = at("> tradebot --config config/testnet.yaml --live --fetch-candles --days 2");
    let failed = at("ดึงแท่งเทียนจริงไม่สำเร็จ");
    let check = at("> tradebot --config config/testnet.yaml --check");
    assert!(
        fetch.is_some() && failed.is_some() && check.is_some(),
        "{output:?}"
    );
    assert!(fetch < failed && failed < check, "{output:?}");
    assert!(
        !output.iter().any(|line| line.starts_with("ยังไม่มีกุญแจ")),
        "keys are there: {output:?}"
    );
    for secret in ["a-key-123", "0xprivate-456"] {
        assert!(
            !output.iter().any(|line| line.contains(secret)),
            "{output:?}"
        );
    }
}
