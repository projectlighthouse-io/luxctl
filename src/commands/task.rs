use color_eyre::eyre::Result;

use crate::api::LighthouseAPIClient;
use crate::config::Config;
use crate::message::Message;
use crate::state::ProjectState;
use crate::ui::UI;

/// handle `luxctl task --task <slug|number> [--detailed]`
pub async fn show(task_id: &str, detailed: bool) -> Result<()> {
    let config = Config::load()?;
    if !config.has_auth_token() {
        UI::error(
            "not authenticated",
            Some("run `luxctl auth --token $token`"),
        );
        return Ok(());
    }

    let state = ProjectState::load(config.expose_token())?;
    let client = LighthouseAPIClient::from_config(&config);

    let project_slug = if let Some(l) = state.get_active() {
        l.slug.clone()
    } else {
        UI::error("no active project", None);
        UI::note("run `luxctl project start --id <ID>` first");
        return Ok(());
    };

    let lab_data = match client.project_by_slug(&project_slug).await {
        Ok(l) => l,
        Err(err) => {
            UI::error(
                &format!("failed to fetch project '{}'", project_slug),
                Some(&format!("{}", err)),
            );
            return Ok(());
        }
    };

    let tasks = if let Some(t) = &lab_data.tasks {
        t
    } else {
        UI::error(&format!("project '{}' has no tasks", project_slug), None);
        return Ok(());
    };

    let listed = if let Ok(task_num) = task_id.parse::<usize>() {
        if task_num == 0 || task_num > tasks.len() {
            UI::error(
                &format!("task #{} not found", task_num),
                Some(&format!("valid range: 1-{}", tasks.len())),
            );
            return Ok(());
        }
        &tasks[task_num - 1]
    } else if let Some(t) = tasks.iter().find(|t| t.slug == task_id) {
        t
    } else {
        UI::error(
            &format!("task '{}' not found in project '{}'", task_id, project_slug),
            None,
        );
        return Ok(());
    };

    // A project response lists its tasks without their briefs, so the prose
    // needs the task's own endpoint. Fetched by uuid, which names exactly one
    // task — a slug names the first project that has one by that name.
    let task_data = match client.task_by_identifier(&listed.id).await {
        Ok(t) => t,
        Err(err) => {
            UI::error(
                &format!("failed to fetch task '{}'", listed.slug),
                Some(&format!("{}", err)),
            );
            return Ok(());
        }
    };

    Message::print_task_detail(&task_data, detailed);

    Ok(())
}
