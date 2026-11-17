//! Tests for how the router is put together: above all, that control mode
//! does not exist unless it was both asked for and safe to offer.

use super::*;
use axum::body::Body;
use axum::http::Request;
use tower::ServiceExt;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("board-lib-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(dir.join("dist")).unwrap();
        std::fs::write(
            dir.join("dist").join("index.html"),
            "<html>the board</html>",
        )
        .unwrap();
        Self(dir)
    }

    fn settings(&self, control: bool) -> Settings {
        Settings {
            dist: self.0.join("dist"),
            logs: self.0.join("logs"),
            candles: self.0.join("candles"),
            code: self.0.clone(),
            config_file: "config/config.yaml".into(),
            utc_offset_hours: 7,
            grvt_env: "testnet".into(),
            account: account::Account::default(),
            control: control.then(|| control::Control::new(&self.0)),
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn loopback() -> SocketAddr {
    "127.0.0.1:4340".parse().unwrap()
}

fn everywhere() -> SocketAddr {
    "0.0.0.0:4340".parse().unwrap()
}

async fn status_of(app: &Router, method: &str, uri: &str, with_header: bool) -> StatusCode {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "127.0.0.1:4340");
    if with_header {
        request = request.header("x-board-control", "1");
    }
    app.clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

const CONTROL_ROUTES: [(&str, &str); 6] = [
    ("GET", "/api/control"),
    ("POST", "/api/control/run"),
    ("POST", "/api/control/stop"),
    ("POST", "/api/control/check"),
    ("POST", "/api/control/resume"),
    ("POST", "/api/control/keys"),
];

#[tokio::test]
async fn with_control_off_none_of_its_routes_exist() {
    let scratch = Scratch::new("off");
    let app = build_router(scratch.settings(false), loopback()).unwrap();
    for (method, uri) in CONTROL_ROUTES {
        assert_eq!(
            status_of(&app, method, uri, true).await,
            StatusCode::NOT_FOUND,
            "{method} {uri} must not exist when control is off"
        );
    }
}

#[tokio::test]
async fn control_asked_for_on_a_public_address_is_refused_not_registered() {
    let scratch = Scratch::new("public");
    // 0.0.0.0 is what Railway binds. Asking for control there changes nothing.
    let app = build_router(scratch.settings(true), everywhere()).unwrap();
    for (method, uri) in CONTROL_ROUTES {
        assert_eq!(
            status_of(&app, method, uri, true).await,
            StatusCode::NOT_FOUND,
            "{method} {uri} must not exist on a public bind"
        );
    }
    assert!(!control_enabled(true, everywhere()));
    assert!(!control_enabled(true, "192.168.1.5:4340".parse().unwrap()));
    assert!(control_enabled(true, loopback()));
    assert!(control_enabled(true, "[::1]:4340".parse().unwrap()));
    assert!(!control_enabled(false, loopback()));
}

#[tokio::test]
async fn control_on_a_loopback_bind_is_registered_and_guarded() {
    let scratch = Scratch::new("on");
    let app = build_router(scratch.settings(true), loopback()).unwrap();
    assert_eq!(
        status_of(&app, "GET", "/api/control", false).await,
        StatusCode::OK
    );
    assert_eq!(
        status_of(&app, "POST", "/api/control/stop", false).await,
        StatusCode::FORBIDDEN,
        "the guard sits in front of every control route"
    );
}

#[tokio::test]
async fn the_read_only_routes_and_the_page_are_unchanged_by_control_mode() {
    let scratch = Scratch::new("same");
    for control in [false, true] {
        let app = build_router(scratch.settings(control), loopback()).unwrap();
        assert_eq!(
            status_of(&app, "GET", "/api/health", false).await,
            StatusCode::OK
        );
        assert_eq!(
            status_of(&app, "GET", "/api/state", false).await,
            StatusCode::OK
        );
        // The single-page app still answers any other path.
        assert_eq!(
            status_of(&app, "GET", "/some/page", false).await,
            StatusCode::OK
        );
    }
}

#[tokio::test]
async fn an_unregistered_api_path_is_a_404_and_not_the_page() {
    let scratch = Scratch::new("404");
    let app = build_router(scratch.settings(false), loopback()).unwrap();
    assert_eq!(
        status_of(&app, "GET", "/api/nothing-here", false).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        status_of(&app, "GET", "/api/state/extra", false).await,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_key_saved_through_the_board_reaches_the_account_cards_without_a_restart() {
    if std::env::var("GRVT_API_KEY").is_ok() {
        return;
    }
    let scratch = Scratch::new("reload");
    let control = control::Control::new(&scratch.0);
    let account = account::Account::from_files("testnet", vec![control.env_path()]);
    assert!(account.keys().is_none());

    let app = control::routes(control, account.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/api/control/keys")
        .header("host", "127.0.0.1:4340")
        .header("x-board-control", "1")
        .body(Body::from(r#"{"api_key": "fresh-api-key-123"}"#))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(account.keys().unwrap().api_key, "fresh-api-key-123");
}
