use color_eyre::eyre::Result;
use colored::Colorize;

use crate::api::LighthouseAPIClient;
use crate::config::Config;
use crate::ui::UI;

/// handle `luxctl hints --task <slug>`
pub async fn list(task_slug: &str) -> Result<()> {
    let config = Config::load()?;
    if !config.has_auth_token() {
        UI::error(
            "not authenticated",
            Some("run `luxctl auth --token $token`"),
        );
        return Ok(());
    }

    let client = LighthouseAPIClient::from_config(&config);

    let hints = match client.hints(task_slug).await {
        Ok(r) => r,
        Err(err) => {
            UI::error("failed to fetch hints", Some(&format!("{}", err)));
            return Ok(());
        }
    };

    if hints.is_empty() {
        UI::info(&format!("no hints available for task '{}'", task_slug));
        return Ok(());
    }

    UI::info(&format!("hints for task: {}", task_slug.bold()));
    UI::blank();

    for (i, hint) in hints.iter().enumerate() {
        if hint.is_unlocked {
            if let Some(text) = &hint.text {
                UI::status_unlocked(i + 1, text, hint.points_deduction);
            }
        } else if hint.is_available {
            let cmd = format!("luxctl hint unlock --task {} --hint {}", task_slug, hint.id);
            UI::status_available(i + 1, hint.points_deduction, &cmd);
        } else {
            UI::status_locked(i + 1, hint.points_deduction);
        }
        UI::blank();
    }

    Ok(())
}

/// handle `luxctl hint unlock --task <slug> --hint <uuid>`
pub async fn unlock(task_slug: &str, hint_id: &str) -> Result<()> {
    let config = Config::load()?;
    if !config.has_auth_token() {
        UI::error(
            "not authenticated",
            Some("run `luxctl auth --token $token`"),
        );
        return Ok(());
    }

    let client = LighthouseAPIClient::from_config(&config);

    let hint = match client.unlock_hint(task_slug, hint_id).await {
        Ok(r) => r,
        Err(err) => {
            UI::error("failed to unlock hint", Some(&format!("{}", err)));
            return Ok(());
        }
    };

    // Unlocking twice costs once, and the api answers 200 either way, so this
    // is not necessarily what took the points.
    UI::success("hint unlocked");
    UI::info(&format!("points deducted: -{}", hint.points_deduction));
    UI::blank();
    UI::info(&format!("{}", "hint:".bold()));
    UI::info(hint.text.as_deref().unwrap_or("(no text)"));

    Ok(())
}
