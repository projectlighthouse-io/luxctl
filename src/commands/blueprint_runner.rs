use std::collections::HashMap;
use std::path::PathBuf;

use color_eyre::eyre::{Result, WrapErr};

use blueprint::executor::context::{Context, ExecutionMode};
use blueprint::executor::Engine;
use blueprint::parser::parse;
use blueprint::reporter::format_api_payload;
use blueprint::transpiler::ir::{Blueprint, Value};
use blueprint::transpiler::{transpile, BlueprintResult, Status};

use crate::api::{SubmitAttemptRequest, TaskOutcome};
use crate::env::{preflight, RunnerImage, Session};
use crate::runtime::SupportedRuntime;
use crate::ui::UI;

/// provision the environment a blueprint's `runner_image` asks for.
///
/// `local|` is what luxctl has always done, so it provisions nothing. `linux|`
/// runs preflight and starts the pinned Linux environment. the returned session
/// must be torn down by the caller once the engine is finished with it.
async fn provision(
    bp: &Blueprint,
    task_slug: &str,
    workspace: Option<&std::path::Path>,
) -> Result<Option<Session>> {
    let image = RunnerImage::parse(bp.meta.runner_image.as_deref());

    if let RunnerImage::Retired(raw) = &image {
        UI::warn(
            "runner",
            Some(&format!(
                "'{raw}' is a retired hosted runner; running locally instead"
            )),
        );
    }

    if !image.is_linux() {
        return Ok(None);
    }

    let reqs = bp.requires_env.as_ref();
    let backend = preflight(reqs).await?;
    let session = Session::start(backend, task_slug, workspace, bp.config.port, reqs).await?;
    Ok(Some(session))
}

/// attach the session's runner and the facts `requires { }` is judged against
fn apply_session(ctx: Context, session: Option<&Session>) -> Context {
    match session {
        Some(s) => ctx.with_runner(s.runner()).with_facts(s.facts()),
        None => ctx,
    }
}

/// build the full build command string from a runtime name.
/// when bin_path is set, Go gets `-o <bin_path>` so the output binary lands at the expected location.
fn build_command_for_runtime(runtime_str: &str, bin_path: Option<&str>) -> Option<String> {
    let rt: SupportedRuntime = runtime_str.parse().ok()?;
    match rt {
        SupportedRuntime::Go => {
            if let Some(path) = bin_path {
                Some(format!("go build -o {} .", path))
            } else {
                Some("go build .".to_string())
            }
        }
        _ => {
            let parts: Vec<&str> = std::iter::once(rt.build_command())
                .chain(rt.build_args())
                .collect();
            Some(parts.join(" "))
        }
    }
}

/// run a blueprint in Validate mode (probe-only, no user inputs)
pub async fn run_validate(
    bp_source: &str,
    task_slug: &str,
    workspace: Option<PathBuf>,
    runtime: Option<&str>,
) -> Result<BlueprintResult> {
    log::debug!("parsing blueprint ({} bytes)...", bp_source.len());
    let ast = parse(bp_source)
        .map_err(|e| color_eyre::eyre::eyre!("parse error at line {}: {}", e.line, e.message))?;
    log::debug!("parsed ok");

    log::debug!("transpiling...");
    let bp = transpile(&ast).map_err(|e| {
        let msg = match e.context {
            Some(ctx) => format!("{}: {}", ctx, e.message),
            None => e.message,
        };
        color_eyre::eyre::eyre!("transpile error: {}", msg)
    })?;
    log::debug!("transpiled ok: {} phases", bp.phases.len());

    let session = provision(&bp, task_slug, workspace.as_deref()).await?;

    let mut ctx = apply_session(
        Context::new(bp.config.clone(), ExecutionMode::Validate),
        session.as_ref(),
    );
    if let Some(ws) = workspace.clone() {
        ctx = ctx.with_workspace(ws);
    }
    if let Some(ref bin) = bp.config.bin {
        ctx.set_variable("$BIN", Value::String(bin.clone()));
    }
    if let Some(rt_str) = runtime {
        let resolved_path = bp
            .config
            .bin_path
            .get(rt_str)
            .cloned()
            .or_else(|| bp.config.bin.as_ref().map(|b| format!("./{}", b)));

        if let Some(ref path) = resolved_path {
            ctx.set_variable("$BIN_PATH", Value::String(path.clone()));
        }

        if let Some(cmd) = build_command_for_runtime(rt_str, resolved_path.as_deref()) {
            ctx.set_variable("$BUILD", Value::String(cmd));
        }
    }
    log::debug!("workspace: {:?}, task: {}", workspace, task_slug);
    let mut engine = Engine::new(ctx).with_task(task_slug);

    log::debug!("executing engine...");
    let result = engine.execute(&bp).await;
    log::debug!("engine done");

    // tear down before propagating, so a failed run does not strand a container
    if let Some(s) = session {
        s.teardown().await;
    }

    result.wrap_err("blueprint execution failed")
}

/// run a blueprint in Result mode (with user inputs for input-matching steps)
pub async fn run_result(
    bp_source: &str,
    task_slug: &str,
    user_inputs: &HashMap<String, String>,
    workspace: Option<PathBuf>,
    runtime: Option<&str>,
) -> Result<BlueprintResult> {
    let ast = parse(bp_source)
        .map_err(|e| color_eyre::eyre::eyre!("parse error at line {}: {}", e.line, e.message))?;

    let bp = transpile(&ast).map_err(|e| {
        let msg = match e.context {
            Some(ctx) => format!("{}: {}", ctx, e.message),
            None => e.message,
        };
        color_eyre::eyre::eyre!("transpile error: {}", msg)
    })?;

    let session = provision(&bp, task_slug, workspace.as_deref()).await?;

    let mut ctx = apply_session(
        Context::new(bp.config.clone(), ExecutionMode::Result),
        session.as_ref(),
    );
    if let Some(ws) = workspace {
        ctx = ctx.with_workspace(ws);
    }
    if let Some(ref bin) = bp.config.bin {
        ctx.set_variable("$BIN", Value::String(bin.clone()));
    }
    if let Some(rt_str) = runtime {
        let resolved_path = bp
            .config
            .bin_path
            .get(rt_str)
            .cloned()
            .or_else(|| bp.config.bin.as_ref().map(|b| format!("./{}", b)));

        if let Some(ref path) = resolved_path {
            ctx.set_variable("$BIN_PATH", Value::String(path.clone()));
        }

        if let Some(cmd) = build_command_for_runtime(rt_str, resolved_path.as_deref()) {
            ctx.set_variable("$BUILD", Value::String(cmd));
        }
    }
    for (key, value) in user_inputs {
        ctx.set_user_input(key, value);
    }

    let mut engine = Engine::new(ctx).with_task(task_slug);

    let result = engine.execute(&bp).await;

    if let Some(s) = session {
        s.teardown().await;
    }

    result.wrap_err("blueprint execution failed")
}

/// convert a BlueprintResult into a SubmitAttemptRequest for the API
pub fn to_attempt_request(
    result: &BlueprintResult,
    project_slug: &str,
    task_id: &str,
) -> SubmitAttemptRequest {
    let outcome = match &result.status {
        Status::Passed => TaskOutcome::Passed,
        Status::Failed => TaskOutcome::Failed,
        _ => TaskOutcome::Failed,
    };

    let payload = format_api_payload(result, None);
    // truncate if too long (API limit ~5000 chars)
    let json = serde_json::to_string(&payload).unwrap_or_default();
    let context = if json.len() > 4900 {
        format!("{}...[truncated]", &json[..4900])
    } else {
        json
    };

    SubmitAttemptRequest {
        project_slug: project_slug.to_string(),
        task_id: task_id.to_string(),
        task_outcome: outcome,
        task_outcome_context: Some(context),
    }
}

/// parse `key=value` CLI args into a HashMap.
/// returns error if any arg is missing the `=` separator.
pub fn parse_inputs(raw: &[String]) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    for item in raw {
        let (key, value) = item.split_once('=').ok_or_else(|| {
            color_eyre::eyre::eyre!("invalid input '{}': expected key=value", item)
        })?;
        map.insert(key.to_string(), value.to_string());
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_parse_inputs_valid() {
        let raw = vec!["key=value".to_string(), "foo=bar".to_string()];
        let result = parse_inputs(&raw).unwrap();
        assert_eq!(result.get("key").unwrap(), "value");
        assert_eq!(result.get("foo").unwrap(), "bar");
    }

    #[test]
    fn test_parse_inputs_with_equals_in_value() {
        let raw = vec!["key=value=extra".to_string()];
        let result = parse_inputs(&raw).unwrap();
        assert_eq!(result.get("key").unwrap(), "value=extra");
    }

    #[test]
    fn test_parse_inputs_missing_equals() {
        let raw = vec!["noequalssign".to_string()];
        let result = parse_inputs(&raw);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_inputs_empty() {
        let raw: Vec<String> = vec![];
        let result = parse_inputs(&raw).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_to_attempt_request_passed() {
        let bp_result = BlueprintResult {
            name: "test".to_string(),
            status: Status::Passed,
            phases: vec![],
            duration_ms: 100,
            captured: HashMap::new(),
            input_provided: HashMap::new(),
        };

        let req = to_attempt_request(
            &bp_result,
            "my-project",
            "a9dccd78-7d42-476f-a9d1-83517426449f",
        );
        assert_eq!(req.project_slug, "my-project");
        assert_eq!(req.task_id, "a9dccd78-7d42-476f-a9d1-83517426449f");
        assert!(matches!(req.task_outcome, TaskOutcome::Passed));
        assert!(req.task_outcome_context.is_some());
    }

    #[test]
    fn test_to_attempt_request_failed() {
        let bp_result = BlueprintResult {
            name: "test".to_string(),
            status: Status::Failed,
            phases: vec![],
            duration_ms: 50,
            captured: HashMap::new(),
            input_provided: HashMap::new(),
        };

        let req = to_attempt_request(&bp_result, "my-lab", "11111111-1111-1111-1111-111111111111");
        assert!(matches!(req.task_outcome, TaskOutcome::Failed));
    }

    #[tokio::test]
    async fn test_run_validate_with_file_probe() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("hello.txt");
        std::fs::write(&file_path, "hello world").unwrap();

        // file probe path is unquoted in the DSL
        let bp_source = format!(
            r#"blueprint "file test" {{
    phase "check" {{
        step "file exists" {{
            probe file {}
            expect {{ exists: true }}
        }}
    }}
}}"#,
            file_path.display()
        );

        let result = run_validate(&bp_source, "test-task", None, None)
            .await
            .unwrap();
        assert!(matches!(result.status, Status::Passed));
    }

    #[tokio::test]
    async fn test_run_validate_fails_on_missing_file() {
        let bp_source = r#"blueprint "missing" {
    phase "check" {
        step "file exists" {
            probe file "/tmp/luxctl-nonexistent-file-test"
            expect { exists: true }
        }
    }
}"#;

        let result = run_validate(bp_source, "test-task", None, None)
            .await
            .unwrap();
        assert!(matches!(result.status, Status::Failed));
    }

    #[tokio::test]
    async fn test_run_validate_parse_error() {
        let result = run_validate("not valid blueprint syntax {{{", "test-task", None, None).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_build_command_go_with_bin_path() {
        let cmd = build_command_for_runtime("go", Some("./seachart"));
        assert_eq!(cmd.as_deref(), Some("go build -o ./seachart ."));
    }

    #[test]
    fn test_build_command_go_without_bin_path() {
        let cmd = build_command_for_runtime("go", None);
        assert_eq!(cmd.as_deref(), Some("go build ."));
    }

    #[test]
    fn test_build_command_rust_ignores_bin_path() {
        let cmd = build_command_for_runtime("rust", Some("./target/debug/seachart"));
        assert_eq!(cmd.as_deref(), Some("cargo build"));
    }

    #[test]
    fn test_build_command_c_ignores_bin_path() {
        let cmd = build_command_for_runtime("c", Some("./seachart"));
        assert_eq!(cmd.as_deref(), Some("make"));
    }

    #[test]
    fn test_build_command_unknown_runtime() {
        let cmd = build_command_for_runtime("python", None);
        assert!(cmd.is_none());
    }
}
