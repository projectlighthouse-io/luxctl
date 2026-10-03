use std::collections::HashSet;
use std::path::PathBuf;

use blueprint::reporter::CliReporter;
use color_eyre::eyre::Result;

use crate::api::{LighthouseAPIClient, SubmitAttemptRequest, Task, TaskOutcome, TaskStatus};
use crate::commands::blueprint_runner;
use crate::config::Config;
use crate::shell;
use crate::state::ProjectState;
use crate::ui::RunUI;
use crate::{complain, oops, say};

/// handle `luxctl run --task <slug|number> [--project <slug>]`
/// task can be specified by slug or by number (1, 01, 2, 02, etc.)
pub async fn run(task_id: &str, project_slug: Option<&str>, detailed: bool) -> Result<()> {
    let config = Config::load()?;
    if !config.has_auth_token() {
        oops!("not authenticated. Run: `luxctl auth --token $token`");
        return Ok(());
    }

    let token = config.expose_token().to_string();
    let mut state = ProjectState::load(&token)?;
    let client = LighthouseAPIClient::from_config(&config);

    // determine project slug (from arg or active project)
    let project_slug = match project_slug {
        Some(s) => s.to_string(),
        None => {
            if let Some(l) = state.get_active() {
                l.slug.clone()
            } else {
                oops!("no project specified and no active project");
                say!("use `--project <ID>` or run `luxctl project start --id <ID>` first");
                return Ok(());
            }
        }
    };

    // fetch project with tasks
    let project_data = match client.project_by_slug(&project_slug).await {
        Ok(l) => l,
        Err(err) => {
            oops!("failed to fetch project '{}': {}", project_slug, err);
            return Ok(());
        }
    };

    // get tasks list
    let tasks = if let Some(t) = &project_data.tasks {
        t
    } else {
        oops!("project '{}' has no tasks", project_slug);
        return Ok(());
    };

    // find task by number or slug
    let task_data = if let Ok(task_num) = task_id.parse::<usize>() {
        // task specified by number (1-based index)
        if task_num == 0 || task_num > tasks.len() {
            oops!(
                "task #{} not found. valid range: 1-{}",
                task_num,
                tasks.len()
            );
            return Ok(());
        }
        &tasks[task_num - 1]
    } else {
        // task specified by slug
        if let Some(t) = tasks.iter().find(|t| t.slug == task_id) {
            t
        } else {
            oops!("task '{}' not found in project '{}'", task_id, project_slug);
            say!("use task number (1, 2, 3...) or slug:");
            for (i, t) in tasks.iter().enumerate() {
                say!("  {:02}. {}", i + 1, t.slug);
            }
            return Ok(());
        }
    };

    let active = state.get_active();
    let workspace = active.map(|l| PathBuf::from(&l.workspace));
    let runtime = active.and_then(|l| l.runtime.clone());

    // collect slugs of previously completed tasks so the reporter
    // can distinguish "already passed" from "never attempted"
    let completed_slugs: HashSet<String> = tasks
        .iter()
        .filter(|t| t.status().is_completed())
        .map(|t| t.slug.clone())
        .collect();

    run_task_validators(
        &client,
        &project_data.slug,
        project_data.blueprint_source(),
        task_data,
        Some((&mut state, &token)),
        workspace,
        detailed,
        &completed_slugs,
        runtime.as_deref(),
    )
    .await
}

/// run the project's blueprint for a single task and submit the result.
/// optionally updates cached state when state_ctx is provided.
///
/// `bp_source` is the *project's* blueprint. there is one per project, and the
/// phase a task runs is picked out of it by the task's slug.
pub async fn run_task_validators(
    client: &LighthouseAPIClient,
    project_slug: &str,
    bp_source: &str,
    task: &Task,
    state_ctx: Option<(&mut ProjectState, &str)>,
    workspace: Option<PathBuf>,
    detailed: bool,
    completed_slugs: &HashSet<String>,
    runtime: Option<&str>,
) -> Result<()> {
    if bp_source.is_empty() {
        let ui = RunUI::new(&task.slug, 0);
        ui.header();
        ui.blank_line();
        ui.step("no blueprint defined for this project");
        return Ok(());
    }

    run_blueprint_task(
        client,
        project_slug,
        task,
        bp_source,
        state_ctx,
        workspace,
        detailed,
        completed_slugs,
        runtime,
    )
    .await
}

/// run a task using the blueprint engine (parse → transpile → execute)
async fn run_blueprint_task(
    client: &LighthouseAPIClient,
    project_slug: &str,
    task: &Task,
    bp_source: &str,
    state_ctx: Option<(&mut ProjectState, &str)>,
    workspace: Option<PathBuf>,
    detailed: bool,
    completed_slugs: &HashSet<String>,
    runtime: Option<&str>,
) -> Result<()> {
    let ui = RunUI::new(&task.slug, 0);

    if task.status().is_completed() {
        complain!("you've already passed this task");
        say!("running blueprint anyway for verification...");
    }

    ui.header();
    ui.blank_line();

    // prologue
    if !task.prologue.is_empty() {
        ui.step(&format!(
            "Running {} setup commands...",
            task.prologue.len()
        ));
        if let Err((cmd, result)) = shell::run_commands(&task.prologue).await {
            oops!("setup command failed: {}", cmd);
            if !result.stderr.is_empty() {
                say!("stderr: {}", result.stderr.trim());
            }
            run_epilogue(&ui, &task.epilogue).await;
            return Ok(());
        }
        ui.blank_line();
    }

    ui.step("Running blueprint...");

    let bp_result =
        match blueprint_runner::run_validate(bp_source, &task.slug, workspace, runtime).await {
            Ok(r) => r,
            Err(err) => {
                oops!("blueprint failed: {}", err);
                run_epilogue(&ui, &task.epilogue).await;
                return Ok(());
            }
        };

    // submit before printing so we can show XP on the summary line
    let attempt_request = blueprint_runner::to_attempt_request(&bp_result, project_slug, &task.id);
    let submission = submit_and_update(client, &attempt_request, task, state_ctx).await;
    let points = submission.as_ref().ok().copied().flatten();

    CliReporter::print_result_with_context(&bp_result, detailed, completed_slugs, points);
    report_unrecorded(&submission);

    run_epilogue(&ui, &task.epilogue).await;
    Ok(())
}

/// submit attempt to API and update local state cache.
/// returns points earned on first-time pass (None otherwise), or why the api
/// refused to record the attempt.
pub async fn submit_and_update(
    client: &LighthouseAPIClient,
    attempt_request: &SubmitAttemptRequest,
    task: &Task,
    state_ctx: Option<(&mut ProjectState, &str)>,
) -> Result<Option<i32>, String> {
    match client.submit_attempt(attempt_request).await {
        Ok(attempt) => {
            log::debug!("attempt recorded: {:?}", attempt);

            let passed = attempt.outcome == TaskOutcome::Passed;

            let points = if attempt.is_reattempt {
                log::debug!("re-attempt recorded (no additional points)");
                None
            } else if passed {
                Some(attempt.points_achieved)
            } else {
                None
            };

            if let Some((state, token)) = state_ctx {
                let new_status = if passed {
                    TaskStatus::ChallengeCompleted
                } else {
                    TaskStatus::Challenged
                };
                state.update_task_status(&task.id, new_status);
                if let Err(e) = state.save(token) {
                    log::warn!("failed to save state: {}", e);
                }
            }

            Ok(points)
        }
        Err(err) => {
            log::error!("failed to submit attempt: {}", err);
            Err(err.to_string())
        }
    }
}

/// say plainly that the run was not recorded, after the summary so it is the
/// last thing on screen. The summary speaks only for the checks: when the api
/// refused the attempt (a 403 `task_locked`, say), "all checks passed" was the
/// last word and read as a pass that counted.
pub fn report_unrecorded(submission: &Result<Option<i32>, String>) {
    if let Err(reason) = submission {
        oops!("  result NOT recorded: {}", reason);
    }
}

/// run epilogue commands with best-effort (continues even on failure)
async fn run_epilogue(ui: &RunUI, commands: &[String]) {
    if commands.is_empty() {
        return;
    }

    ui.blank_line();
    ui.step(&format!("Running {} cleanup commands...", commands.len()));

    let failures = shell::run_commands_best_effort(commands).await;
    for (cmd, result) in failures {
        log::warn!(
            "cleanup command failed: {} (exit {})",
            cmd,
            result.exit_code
        );
        if !result.stderr.is_empty() {
            log::debug!("stderr: {}", result.stderr.trim());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::TaskInputType;

    fn make_task_with_hooks(prologue: Vec<String>, epilogue: Vec<String>) -> Task {
        Task {
            id: "a9dccd78-7d42-476f-a9d1-83517426449f".to_string(),
            slug: "test-task".to_string(),
            title: "Test Task".to_string(),
            description: Some("A test task".to_string()),
            sort_order: 1,
            input_type: TaskInputType::None,
            scores: Some("10:20:50".to_string()),
            points: 50,
            is_free: false,
            abandoned_deduction: 5,
            hints: vec![],
            progress: None,
            prologue,
            epilogue,
        }
    }

    #[test]
    fn test_task_with_empty_hooks() {
        let task = make_task_with_hooks(vec![], vec![]);
        assert!(task.prologue.is_empty());
        assert!(task.epilogue.is_empty());
    }

    #[test]
    fn test_task_with_prologue_and_epilogue() {
        let task = make_task_with_hooks(
            vec!["docker compose up -d".to_string()],
            vec!["docker compose down".to_string()],
        );

        assert_eq!(task.prologue.len(), 1);
        assert_eq!(task.epilogue.len(), 1);
        assert_eq!(task.prologue[0], "docker compose up -d");
        assert_eq!(task.epilogue[0], "docker compose down");
    }

    #[tokio::test]
    async fn test_prologue_stops_on_failure() {
        let commands = vec![
            "echo starting".to_string(),
            "exit 1".to_string(),
            "echo should not run".to_string(),
        ];

        let result = shell::run_commands(&commands).await;
        assert!(result.is_err());

        let (failed_cmd, _) = result.unwrap_err();
        assert_eq!(failed_cmd, "exit 1");
    }

    #[tokio::test]
    async fn test_epilogue_continues_on_failure() {
        let commands = vec![
            "exit 1".to_string(),
            "exit 2".to_string(),
            "echo still runs".to_string(),
        ];

        // best_effort continues even when commands fail
        let failures = shell::run_commands_best_effort(&commands).await;

        // should have 2 failures (exit 1 and exit 2)
        assert_eq!(failures.len(), 2);
    }

    #[tokio::test]
    async fn test_prologue_success_allows_continuation() {
        let commands = vec!["echo one".to_string(), "echo two".to_string()];

        let result = shell::run_commands(&commands).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_refused_attempt_is_reported_as_not_recorded() {
        use crate::api::TaskOutcome;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // a stand-in api that refuses the attempt the way the real one does
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request).await;
            let body = r#"{"code":"task_locked","error":"Finish the task before this one first."}"#;
            let response = format!(
                "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        });

        let client =
            LighthouseAPIClient::for_tests(&format!("http://127.0.0.1:{port}"), "token").unwrap();
        let task = make_task_with_hooks(vec![], vec![]);
        let request = SubmitAttemptRequest {
            project_slug: "p".to_string(),
            task_id: task.id.clone(),
            task_outcome: TaskOutcome::Passed,
            task_outcome_context: None,
        };

        let reason = submit_and_update(&client, &request, &task, None)
            .await
            .unwrap_err();
        assert!(reason.contains("403"), "{reason}");
        assert!(reason.contains("task_locked"), "{reason}");
        assert!(
            reason.contains("Finish the task before this one first."),
            "{reason}"
        );
    }
}
