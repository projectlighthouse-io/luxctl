use std::collections::HashSet;
use std::path::PathBuf;

use blueprint::reporter::CliReporter;
use color_eyre::eyre::Result;

use crate::api::LighthouseAPIClient;
use crate::commands::blueprint_runner;
use crate::commands::run::{report_unrecorded, submit_and_update};
use crate::config::Config;
use crate::shell;
use crate::state::ProjectState;
use crate::ui::RunUI;
use crate::{oops, say};

/// handle `luxctl result --task <slug|number> --input key=value`
/// runs blueprint in Result mode, matching user-provided inputs against captured values
pub async fn result(task_id: &str, inputs: &[String], project_slug: Option<&str>) -> Result<()> {
    let config = Config::load()?;
    if !config.has_auth_token() {
        oops!("not authenticated. Run: `luxctl auth --token $token`");
        return Ok(());
    }

    let token = config.expose_token().to_string();
    let mut state = ProjectState::load(&token)?;
    let client = LighthouseAPIClient::from_config(&config);

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

    let lab_data = match client.project_by_slug(&project_slug).await {
        Ok(l) => l,
        Err(err) => {
            oops!("failed to fetch project '{}': {}", project_slug, err);
            return Ok(());
        }
    };

    let tasks = if let Some(t) = &lab_data.tasks {
        t
    } else {
        oops!("project '{}' has no tasks", project_slug);
        return Ok(());
    };

    let task_data = if let Ok(task_num) = task_id.parse::<usize>() {
        if task_num == 0 || task_num > tasks.len() {
            oops!(
                "task #{} not found. valid range: 1-{}",
                task_num,
                tasks.len()
            );
            return Ok(());
        }
        &tasks[task_num - 1]
    } else if let Some(t) = tasks.iter().find(|t| t.slug == task_id) {
        t
    } else {
        oops!("task '{}' not found in project '{}'", task_id, project_slug);
        return Ok(());
    };

    // The blueprint belongs to the project, not the task: one file, and the
    // phase this task runs is picked out of it by slug.
    if !lab_data.has_blueprint() {
        oops!(
            "project '{}' has no blueprint — nothing to run",
            project_slug
        );
        return Ok(());
    }

    let bp_source = lab_data.blueprint_source();
    let user_inputs = blueprint_runner::parse_inputs(inputs)?;
    let active = state.get_active();
    let workspace = active.map(|l| PathBuf::from(&l.workspace));
    let runtime = active.and_then(|l| l.runtime.clone());

    let ui = RunUI::new(&task_data.slug, 0);
    ui.header();
    ui.blank_line();

    // prologue
    if !task_data.prologue.is_empty() {
        ui.step(&format!(
            "Running {} setup commands...",
            task_data.prologue.len()
        ));
        if let Err((cmd, shell_result)) = shell::run_commands(&task_data.prologue).await {
            oops!("setup command failed: {}", cmd);
            if !shell_result.stderr.is_empty() {
                say!("stderr: {}", shell_result.stderr.trim());
            }
            return Ok(());
        }
        ui.blank_line();
    }

    ui.step("Running blueprint (result mode)...");

    let bp_result = match blueprint_runner::run_result(
        bp_source,
        &task_data.slug,
        &user_inputs,
        workspace,
        runtime.as_deref(),
    )
    .await
    {
        Ok(r) => r,
        Err(err) => {
            oops!("blueprint failed: {}", err);
            return Ok(());
        }
    };

    // submit before printing so we can show XP on the summary line
    let attempt_request =
        blueprint_runner::to_attempt_request(&bp_result, &project_slug, &task_data.id);
    let submission = submit_and_update(
        &client,
        &attempt_request,
        task_data,
        Some((&mut state, &token)),
    )
    .await;
    let points = submission.as_ref().ok().copied().flatten();

    CliReporter::print_result_with_context(&bp_result, false, &HashSet::new(), points);
    report_unrecorded(&submission);

    // epilogue
    if !task_data.epilogue.is_empty() {
        ui.step(&format!(
            "Running {} cleanup commands...",
            task_data.epilogue.len()
        ));
        let failures = shell::run_commands_best_effort(&task_data.epilogue).await;
        for (cmd, r) in failures {
            log::warn!("cleanup failed: {} (exit {})", cmd, r.exit_code);
        }
    }

    Ok(())
}
