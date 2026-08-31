//! E2E tests against local projectlighthouse.io API
//!
//! Prerequisites:
//! 1. Run projectlighthouse.io locally: `composer run dev`
//! 2. Have a valid test user with API token
//!
//! Run with: LUXCTL_E2E_TOKEN=<token> cargo test --test e2e -- --nocapture
//!
//! Note: Uses 0.0.0.0 instead of localhost to avoid IPv6 resolution issues

use std::path::Path;
use std::process::{Command, Output};

const API_BASE_URL: &str = "http://0.0.0.0:8000";

fn get_test_token() -> Option<String> {
    // try dev_token file first (gitignored, local dev)
    let dev_token_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("dev_token");
    if let Ok(token) = std::fs::read_to_string(&dev_token_path) {
        let token = token.trim().to_string();
        if !token.is_empty() {
            return Some(token);
        }
    }

    // fallback to env var
    std::env::var("LUXCTL_E2E_TOKEN").ok()
}

fn luxctl(args: &[&str]) -> Output {
    Command::new("cargo")
        .args(["run", "--quiet", "--"])
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("LUXCTL_ENV", "DEV")
        .env("LUXCTL_API_BASE_URL", API_BASE_URL)
        .output()
        .expect("failed to execute luxctl")
}

fn luxctl_with_token(args: &[&str], token: &str) -> Output {
    // first authenticate, then run command
    let _ = luxctl(&["auth", "--token", token]);
    luxctl(args)
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

// helper to check if local API is running
fn api_is_running() -> bool {
    std::net::TcpStream::connect("0.0.0.0:8000").is_ok()
}

fn require_api() {
    if !api_is_running() {
        panic!(
            "Local API not running at {}. Start it with: cd projectlighthouse.io && composer run dev",
            API_BASE_URL
        );
    }
}

fn require_token() -> String {
    get_test_token()
        .expect("Token required. Add to dev_token file or set LUXCTL_E2E_TOKEN env var.")
}

#[test]
#[ignore] // run with: cargo test --test e2e -- --ignored
fn e2e_auth_with_valid_token() {
    require_api();
    let token = require_token();

    let output = luxctl(&["auth", "--token", &token]);

    assert!(output.status.success(), "auth failed: {}", stderr(&output));
    let out = stdout(&output);
    assert!(
        out.contains("Welcome") || out.contains("welcome"),
        "expected welcome message, got: {}",
        out
    );
}

#[test]
#[ignore]
fn e2e_auth_with_invalid_token() {
    require_api();

    let output = luxctl(&["auth", "--token", "invalid-token-12345"]);

    // should fail gracefully
    let combined = format!("{}{}", stdout(&output), stderr(&output)).to_lowercase();
    assert!(
        combined.contains("invalid")
            || combined.contains("unauthorized")
            || combined.contains("unauthenticated")
            || combined.contains("failed"),
        "expected error message for invalid token, got: {}",
        combined
    );
}

#[test]
#[ignore]
fn e2e_whoami_authenticated() {
    require_api();
    let token = require_token();

    let output = luxctl_with_token(&["whoami"], &token);

    assert!(
        output.status.success(),
        "whoami failed: {}",
        stderr(&output)
    );
    let out = stdout(&output);
    assert!(!out.is_empty(), "expected user info, got empty output");
}

#[test]
#[ignore]
fn e2e_whoami_unauthenticated() {
    require_api();

    // clear any existing auth by using a fresh home dir
    let output = Command::new("cargo")
        .args(["run", "--quiet", "--", "whoami"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("LUXCTL_ENV", "DEV")
        .env("LUXCTL_API_BASE_URL", API_BASE_URL)
        .env("HOME", "/tmp/luxctl-e2e-test-no-auth")
        .output()
        .expect("failed to execute luxctl");

    let out = stdout(&output);
    assert!(
        out.contains("nobody") || out.contains("login"),
        "expected 'nobody' or login prompt, got: {}",
        out
    );
}

#[test]
#[ignore]
fn e2e_project_list() {
    require_api();
    let token = require_token();

    let output = luxctl_with_token(&["project", "list"], &token);

    assert!(
        output.status.success(),
        "project list failed: {}",
        stderr(&output)
    );
    // should show at least some output (labs or empty message)
    let out = stdout(&output);
    assert!(!out.is_empty(), "expected project list output");
}

#[test]
#[ignore]
fn e2e_project_show() {
    require_api();
    let token = require_token();

    // use 1brc as the test project
    let output = luxctl_with_token(&["project", "show", "--slug", "1brc"], &token);

    let err = stderr(&output);

    // either shows project details or "not found" - both are valid responses
    assert!(
        output.status.success() || err.contains("not found"),
        "unexpected error: {}",
        err
    );
}

#[test]
#[ignore]
fn e2e_project_lifecycle() {
    require_api();
    let token = require_token();

    // 1. start a project
    let output = luxctl_with_token(
        &[
            "project",
            "start",
            "--id",
            "1-billion-row-challenge",
            "--workspace",
            "/tmp/luxctl-e2e-workspace",
        ],
        &token,
    );
    let out = stdout(&output);
    let err = stderr(&output);

    if !output.status.success() && !err.contains("not found") {
        panic!("project start failed unexpectedly: {}", err);
    }

    if output.status.success() {
        assert!(
            out.contains("now working on") || out.contains("1 Billion Row Challenge"),
            "expected confirmation, got: {}",
            out
        );

        // 2. check status
        let output = luxctl(&["project", "status"]);
        assert!(
            output.status.success(),
            "project status failed: {}",
            stderr(&output)
        );
        let out = stdout(&output);
        assert!(
            out.contains("1-billion-row-challenge") || out.contains("active"),
            "expected active project info, got: {}",
            out
        );

        // 3. list tasks
        let output = luxctl(&["task", "list"]);
        assert!(
            output.status.success(),
            "task list failed: {}",
            stderr(&output)
        );

        // 4. restart project
        let output = luxctl(&["project", "restart"]);
        assert!(
            output.status.success() || stdout(&output).contains("restarted"),
            "project restart failed: {}",
            stderr(&output)
        );

        // 5. stop project
        let output = luxctl(&["project", "stop"]);
        assert!(
            output.status.success(),
            "project stop failed: {}",
            stderr(&output)
        );
        let out = stdout(&output);
        assert!(
            out.contains("stopped") || out.contains("no active project"),
            "expected stop confirmation, got: {}",
            out
        );
    }
}

#[test]
#[ignore]
fn e2e_task_show() {
    require_api();
    let token = require_token();

    // start a project first
    let _ = luxctl_with_token(
        &[
            "project",
            "start",
            "--id",
            "1-billion-row-challenge",
            "--workspace",
            "/tmp/luxctl-e2e-workspace",
        ],
        &token,
    );

    // try to show first task
    let output = luxctl(&["task", "show", "--task", "1"]);

    // might succeed or fail depending on project state, but shouldn't crash
    let combined = format!("{}{}", stdout(&output), stderr(&output));
    assert!(!combined.is_empty(), "expected some output from task show");

    // cleanup
    let _ = luxctl(&["project", "stop"]);
}

#[test]
#[ignore]
fn e2e_doctor() {
    require_api();

    let output = luxctl(&["doctor"]);

    assert!(
        output.status.success(),
        "doctor failed: {}",
        stderr(&output)
    );
    // doctor should output some diagnostics
    assert!(!stdout(&output).is_empty(), "expected doctor output");
}
