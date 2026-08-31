use color_eyre::eyre::Result;

use crate::api::LighthouseAPIClient;
use crate::config::Config;
use crate::projectfiles;
use crate::state::ProjectState;
use crate::ui::UI;

/// handle `luxctl project start --id <id> --workspace <path> [--runtime <runtime>]`
pub async fn start(slug: &str, workspace: &str, runtime: Option<&str>) -> Result<()> {
    let config = Config::load()?;
    if !config.has_auth_token() {
        UI::error(
            "not authenticated",
            Some("run `luxctl auth --token $token`"),
        );
        return Ok(());
    }

    let client = LighthouseAPIClient::from_config(&config);

    let project = match client.project_by_slug(slug).await {
        Ok(l) => l,
        Err(err) => {
            UI::error(
                &format!("project '{}' not found", slug),
                Some(&format!("{}", err)),
            );
            UI::note("run `luxctl project list` to see available projects");
            return Ok(());
        }
    };

    let workspace_path = std::path::Path::new(workspace);
    let absolute_workspace = if workspace_path.is_absolute() {
        workspace_path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| color_eyre::eyre::eyre!("cannot get cwd: {}", e))?
            .join(workspace_path)
    };

    let canonical = absolute_workspace
        .canonicalize()
        .unwrap_or(absolute_workspace);

    let workspace_str = canonical.to_string_lossy().to_string();

    let tasks = project.tasks.as_deref().unwrap_or(&[]);

    let mut state = ProjectState::load(config.expose_token())?;
    state.set_active(&project.slug, &project.name, tasks, &workspace_str, runtime);
    state.save(config.expose_token())?;

    UI::success(&format!("now working on: {}", project.name));
    UI::kv("workspace", &workspace_str);
    if let Some(rt) = runtime {
        UI::kv("runtime", rt);
    }

    // fetch fixture files from projectfiles repo
    match projectfiles::download_fixtures(&project.slug, &canonical).await {
        Ok(true) => {
            UI::success("project files synced");
        }
        Ok(false) => {
            log::debug!("no fixtures available for '{}'", project.slug);
        }
        Err(e) => {
            log::debug!("fixture download failed: {}", e);
            UI::note("run `luxctl sync` to retry downloading project files");
        }
    }

    UI::note("run `luxctl task list` to see available tasks");

    Ok(())
}

/// handle `luxctl project status`
pub fn status() -> Result<()> {
    let config = Config::load()?;
    if !config.has_auth_token() {
        UI::error(
            "not authenticated",
            Some("run `luxctl auth --token $token`"),
        );
        return Ok(());
    }

    let state = ProjectState::load(config.expose_token())?;

    if let Some(project) = state.get_active() {
        UI::kv_aligned("active project", &project.name, 16);
        UI::kv_aligned("slug", &project.slug, 16);
        UI::kv_aligned("workspace", &project.workspace, 16);
        if let Some(ref rt) = project.runtime {
            UI::kv_aligned("runtime", rt, 16);
        } else {
            UI::kv_aligned("runtime", "not set", 16);
        }
        UI::kv_aligned(
            "progress",
            &format!(
                "{}/{} tasks completed",
                project.completed_count(),
                project.tasks.len()
            ),
            16,
        );
        UI::note("run `luxctl task list` for task list");
    } else {
        UI::info("no active project");
        UI::note("run `luxctl project start --id <ID>` to start one");
    }

    Ok(())
}

/// handle `luxctl project stop`
pub fn stop() -> Result<()> {
    let config = Config::load()?;
    if !config.has_auth_token() {
        UI::error(
            "not authenticated",
            Some("run `luxctl auth --token $token`"),
        );
        return Ok(());
    }

    let mut state = ProjectState::load(config.expose_token())?;

    if state.get_active().is_some() {
        let name = state
            .get_active()
            .map(|l| l.name.clone())
            .unwrap_or_default();
        state.clear_active();
        state.save(config.expose_token())?;
        UI::success(&format!("stopped working on: {}", name));
    } else {
        UI::info("no active project to stop");
    }

    Ok(())
}

/// handle `luxctl project set --runtime <runtime>`
pub fn set_runtime(runtime: &str) -> Result<()> {
    let config = Config::load()?;
    if !config.has_auth_token() {
        UI::error(
            "not authenticated",
            Some("run `luxctl auth --token $token`"),
        );
        return Ok(());
    }

    let mut state = ProjectState::load(config.expose_token())?;

    if state.get_active().is_some() {
        state.set_runtime(runtime);
        state.save(config.expose_token())?;
        UI::success(&format!("runtime set to: {}", runtime));
    } else {
        UI::error("no active project", None);
        UI::note("run `luxctl project start --id <ID>` first");
    }

    Ok(())
}

/// handle `luxctl project set --workspace <path>`
pub fn set_workspace(workspace: &str) -> Result<()> {
    let config = Config::load()?;
    if !config.has_auth_token() {
        UI::error(
            "not authenticated",
            Some("run `luxctl auth --token $token`"),
        );
        return Ok(());
    }

    let mut state = ProjectState::load(config.expose_token())?;

    if state.get_active().is_none() {
        UI::error("no active project", None);
        UI::note("run `luxctl project start --id <ID>` first");
        return Ok(());
    }

    let workspace_path = std::path::Path::new(workspace);
    let absolute_workspace = if workspace_path.is_absolute() {
        workspace_path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| color_eyre::eyre::eyre!("cannot get cwd: {}", e))?
            .join(workspace_path)
    };

    if !absolute_workspace.exists() {
        UI::error(
            "directory does not exist",
            Some(&absolute_workspace.to_string_lossy()),
        );
        return Ok(());
    }

    let canonical = absolute_workspace
        .canonicalize()
        .map_err(|e| color_eyre::eyre::eyre!("cannot resolve path: {}", e))?;

    let workspace_str = canonical.to_string_lossy().to_string();
    state.set_workspace(&workspace_str);
    state.save(config.expose_token())?;
    UI::success(&format!("workspace set to: {}", workspace_str));

    Ok(())
}

/// handle `luxctl project restart`
pub async fn restart() -> Result<()> {
    let config = Config::load()?;
    if !config.has_auth_token() {
        UI::error(
            "not authenticated",
            Some("run `luxctl auth --token $token`"),
        );
        return Ok(());
    }

    let mut state = ProjectState::load(config.expose_token())?;

    let project = match state.get_active() {
        Some(l) => l.clone(),
        None => {
            UI::error("no active project", None);
            UI::note("run `luxctl project start --id <ID>` first");
            return Ok(());
        }
    };

    let client = LighthouseAPIClient::from_config(&config);

    match client.restart_project(&project.slug).await {
        Ok(response) => {
            // refresh tasks from server
            match client.project_by_slug(&project.slug).await {
                Ok(refreshed) => {
                    let tasks = refreshed.tasks.as_deref().unwrap_or(&[]);
                    state.set_active(
                        &project.slug,
                        &project.name,
                        tasks,
                        &project.workspace,
                        project.runtime.as_deref(),
                    );
                    state.save(config.expose_token())?;
                }
                Err(_) => {
                    // if refresh fails, just clear local progress
                    state.clear_progress();
                    state.save(config.expose_token())?;
                }
            }

            UI::success(&format!("restarted project: {}", project.name));
            UI::info(&format!("you are now on run {}", response.run));
        }
        Err(err) => {
            UI::error("failed to restart project", Some(&format!("{}", err)));
        }
    }

    Ok(())
}
