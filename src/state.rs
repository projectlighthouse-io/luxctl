use chrono::{DateTime, Utc};
use color_eyre::eyre::{self, Ok};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::{fs, path::PathBuf};

use crate::api::{Task, TaskStatus};

static CFG_DIR: &str = ".luxctl";
static STATE_FILE: &str = "state.json";

// salt used for HMAC key derivation (combined with user token)
static HMAC_SALT: &str = "luxctl-state-integrity-v1";

type HmacSha256 = Hmac<Sha256>;

/// task data cached for offline access and integrity protection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedTask {
    /// a uuid. the integer ids are gone — see `api::types`.
    pub id: String,
    pub slug: String,
    pub title: String,
    pub points: i32,
    #[serde(default)]
    pub points_earned: i32,
    pub status: TaskStatus,
    pub sort_order: i32,
}

impl CachedTask {
    /// create from API task, extracting base points from scores string
    pub fn from_api_task(task: &Task) -> Self {
        // scores format: "attempts:minutes:points|..." - take max points from first tier
        let points = task
            .scores()
            .split('|')
            .next()
            .and_then(|tier| tier.split(':').nth(2))
            .and_then(|p| p.parse().ok())
            .unwrap_or(task.points);

        CachedTask {
            id: task.id.clone(),
            slug: task.slug.clone(),
            title: task.title.clone(),
            points,
            points_earned: task.points_earned(),
            status: task.status(),
            sort_order: task.sort_order,
        }
    }
}

/// active project with cached task data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveProject {
    pub slug: String,
    pub name: String,
    pub fetched_at: DateTime<Utc>,
    pub tasks: Vec<CachedTask>,
    #[serde(default = "default_workspace")]
    pub workspace: String,
    #[serde(default)]
    pub runtime: Option<String>,
}

fn default_workspace() -> String {
    ".".to_string()
}

impl ActiveProject {
    pub fn total_points(&self) -> i32 {
        self.tasks.iter().map(|t| t.points).sum()
    }

    pub fn earned_points(&self) -> i32 {
        self.tasks.iter().map(|t| t.points_earned).sum()
    }

    pub fn completed_count(&self) -> usize {
        self.tasks
            .iter()
            .filter(|t| t.status.is_completed())
            .count()
    }
}

/// internal state file format (includes checksum)
#[derive(Debug, Serialize, Deserialize)]
struct StateFile {
    active_project: Option<ActiveProject>,
    checksum: String,
}

/// project state manager with tamper detection
#[derive(Debug)]
pub struct ProjectState {
    pub active_project: Option<ActiveProject>,
}

impl ProjectState {
    /// create empty state
    pub fn new() -> Self {
        ProjectState {
            active_project: None,
        }
    }

    /// load state from disk, verifying integrity with HMAC
    /// if checksum fails, returns empty state (forces re-fetch)
    pub fn load(token: &str) -> eyre::Result<Self> {
        let path = Self::state_path()?;

        if !path.exists() {
            return Ok(ProjectState::new());
        }

        let content = fs::read_to_string(&path)
            .map_err(|e| eyre::eyre!("failed to read state file: {}", e))?;

        // A state file written by an older luxctl holds integer task ids. It
        // cannot be read, and erroring here would break every command until
        // the reader found the file and deleted it — so it is cleared and
        // re-fetched, exactly as a checksum mismatch is.
        let Some(state_file) = serde_json::from_str::<StateFile>(&content)
            .inspect_err(|e| {
                log::warn!("state file is from an older luxctl ({e}), clearing state");
            })
            .ok()
        else {
            let empty = ProjectState::new();
            empty.save(token)?;
            return Ok(empty);
        };

        // verify checksum
        let expected = Self::compute_checksum(&state_file.active_project, token);
        if state_file.checksum != expected {
            log::warn!("state file checksum mismatch, clearing state");
            // tampered or token changed - clear state
            let empty = ProjectState::new();
            empty.save(token)?;
            return Ok(empty);
        }

        Ok(ProjectState {
            active_project: state_file.active_project,
        })
    }

    /// save state to disk with HMAC checksum
    pub fn save(&self, token: &str) -> eyre::Result<()> {
        let path = Self::state_path()?;

        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }

        let checksum = Self::compute_checksum(&self.active_project, token);
        let state_file = StateFile {
            active_project: self.active_project.clone(),
            checksum,
        };

        let content = serde_json::to_string_pretty(&state_file)
            .map_err(|e| eyre::eyre!("failed to serialize state: {}", e))?;

        fs::write(&path, content)?;
        log::debug!("state saved to {}", path.display());

        Ok(())
    }

    /// set active project from API data
    pub fn set_active(
        &mut self,
        slug: &str,
        name: &str,
        tasks: &[Task],
        workspace: &str,
        runtime: Option<&str>,
    ) {
        let cached_tasks: Vec<CachedTask> = tasks.iter().map(CachedTask::from_api_task).collect();

        self.active_project = Some(ActiveProject {
            slug: slug.to_string(),
            name: name.to_string(),
            fetched_at: Utc::now(),
            tasks: cached_tasks,
            workspace: workspace.to_string(),
            runtime: runtime.map(|s| s.to_string()),
        });
    }

    /// apply a mutation to the active project if one exists
    fn with_active_mut<F>(&mut self, f: F)
    where
        F: FnOnce(&mut ActiveProject),
    {
        if let Some(ref mut project) = self.active_project {
            f(project);
        }
    }

    /// update runtime for active project
    pub fn set_runtime(&mut self, runtime: &str) {
        self.with_active_mut(|l| l.runtime = Some(runtime.to_string()));
    }

    /// update workspace for active project
    pub fn set_workspace(&mut self, workspace: &str) {
        self.with_active_mut(|l| l.workspace = workspace.to_string());
    }

    /// clear active project
    pub fn clear_active(&mut self) {
        self.active_project = None;
    }

    /// get reference to active project
    pub fn get_active(&self) -> Option<&ActiveProject> {
        self.active_project.as_ref()
    }

    /// update cached tasks (for refresh)
    pub fn refresh_tasks(&mut self, tasks: &[Task]) {
        self.with_active_mut(|l| {
            l.tasks = tasks.iter().map(CachedTask::from_api_task).collect();
            l.fetched_at = Utc::now();
        });
    }

    /// update a single task's status (e.g., after successful submission)
    pub fn update_task_status(&mut self, task_id: &str, new_status: TaskStatus) {
        self.with_active_mut(|l| {
            if let Some(task) = l.tasks.iter_mut().find(|t| t.id == task_id) {
                task.status = new_status;
            }
        });
    }

    /// reset all task progress to initial state (for project restart)
    pub fn clear_progress(&mut self) {
        self.with_active_mut(|l| {
            for task in &mut l.tasks {
                task.status = TaskStatus::ChallengeAwaits;
                task.points_earned = 0;
            }
        });
    }

    /// compute HMAC-SHA256 checksum of project data
    /// returns empty string if HMAC creation fails (should never happen for SHA256)
    fn compute_checksum(lab: &Option<ActiveProject>, token: &str) -> String {
        // derive key from token + salt
        let key = format!("{}{}", token, HMAC_SALT);

        // HMAC-SHA256 accepts any key length, so this should never fail
        let Some(mut mac) = HmacSha256::new_from_slice(key.as_bytes()).ok() else {
            log::error!("failed to create HMAC - this should never happen");
            return String::new();
        };

        // hash the lab data as JSON
        let data = serde_json::to_string(lab).unwrap_or_default();
        mac.update(data.as_bytes());

        let result = mac.finalize();
        hex::encode(result.into_bytes())
    }

    fn state_path() -> eyre::Result<PathBuf> {
        let home =
            dirs::home_dir().ok_or_else(|| eyre::eyre!("could not determine home directory"))?;

        Ok(home.join(CFG_DIR).join(STATE_FILE))
    }
}

impl Default for ProjectState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{TaskInputType, TaskProgress};

    fn test_token() -> &'static str {
        "test-secret-token-123"
    }

    #[test]
    fn test_cached_task_from_api_task() {
        let api_task = Task {
            id: "a9dccd78-7d42-476f-a9d1-83517426449f".to_string(),
            slug: "test-task".to_string(),
            title: "Test Task".to_string(),
            description: Some("Description".to_string()),
            sort_order: 1,
            input_type: TaskInputType::None,
            scores: Some("5:10:50|10:20:35".to_string()),
            points: 50,
            is_free: false,
            abandoned_deduction: 5,
            hints: vec![],
            progress: Some(TaskProgress {
                status: TaskStatus::ChallengeAwaits,
                points_earned: 35,
                ..TaskProgress::default()
            }),
            prologue: vec![],
            epilogue: vec![],
        };

        let cached = CachedTask::from_api_task(&api_task);

        assert_eq!(cached.id, "a9dccd78-7d42-476f-a9d1-83517426449f");
        assert_eq!(cached.slug, "test-task");
        assert_eq!(cached.points, 50); // max points from first tier
        assert_eq!(cached.points_earned, 35);
    }

    #[test]
    fn test_cached_task_from_a_signed_out_task_has_no_progress() {
        // no `progress` on the wire: an anonymous listing. every derived
        // number is the untouched one rather than a panic or a stale value.
        let api_task = Task {
            id: "a9dccd78-7d42-476f-a9d1-83517426449f".to_string(),
            slug: "test-task".to_string(),
            title: "Test Task".to_string(),
            description: None,
            sort_order: 1,
            input_type: TaskInputType::None,
            scores: None,
            points: 25,
            is_free: true,
            abandoned_deduction: 5,
            hints: vec![],
            progress: None,
            prologue: vec![],
            epilogue: vec![],
        };

        let cached = CachedTask::from_api_task(&api_task);

        // no ladder to read, so the task's flat points stand
        assert_eq!(cached.points, 25);
        assert_eq!(cached.points_earned, 0);
        assert_eq!(cached.status, TaskStatus::ChallengeAwaits);
    }

    #[test]
    fn test_compute_checksum_deterministic() {
        let lab = Some(ActiveProject {
            slug: "test".to_string(),
            name: "Test Lab".to_string(),
            fetched_at: DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .expect("valid date")
                .with_timezone(&Utc),
            tasks: vec![],
            workspace: ".".to_string(),
            runtime: None,
        });

        let checksum1 = ProjectState::compute_checksum(&lab, test_token());
        let checksum2 = ProjectState::compute_checksum(&lab, test_token());

        assert_eq!(checksum1, checksum2);
    }

    #[test]
    fn test_checksum_changes_with_data() {
        let lab1 = Some(ActiveProject {
            slug: "test1".to_string(),
            name: "Test Lab".to_string(),
            fetched_at: Utc::now(),
            tasks: vec![],
            workspace: ".".to_string(),
            runtime: None,
        });

        let lab2 = Some(ActiveProject {
            slug: "test2".to_string(),
            name: "Test Lab".to_string(),
            fetched_at: Utc::now(),
            tasks: vec![],
            workspace: ".".to_string(),
            runtime: None,
        });

        let checksum1 = ProjectState::compute_checksum(&lab1, test_token());
        let checksum2 = ProjectState::compute_checksum(&lab2, test_token());

        assert_ne!(checksum1, checksum2);
    }

    #[test]
    fn test_checksum_changes_with_token() {
        let lab = Some(ActiveProject {
            slug: "test".to_string(),
            name: "Test Lab".to_string(),
            fetched_at: Utc::now(),
            tasks: vec![],
            workspace: ".".to_string(),
            runtime: None,
        });

        let checksum1 = ProjectState::compute_checksum(&lab, "token1");
        let checksum2 = ProjectState::compute_checksum(&lab, "token2");

        assert_ne!(checksum1, checksum2);
    }

    #[test]
    fn test_active_project_stats() {
        let lab = ActiveProject {
            slug: "test".to_string(),
            name: "Test".to_string(),
            fetched_at: Utc::now(),
            tasks: vec![
                CachedTask {
                    id: "11111111-1111-1111-1111-111111111111".to_string(),
                    slug: "t1".to_string(),
                    title: "Task 1".to_string(),
                    points: 25,
                    points_earned: 20,
                    status: TaskStatus::ChallengeCompleted,
                    sort_order: 1,
                },
                CachedTask {
                    id: "22222222-2222-2222-2222-222222222222".to_string(),
                    slug: "t2".to_string(),
                    title: "Task 2".to_string(),
                    points: 50,
                    points_earned: 0,
                    status: TaskStatus::ChallengeAwaits,
                    sort_order: 2,
                },
            ],
            workspace: ".".to_string(),
            runtime: Some("go".to_string()),
        };

        assert_eq!(lab.total_points(), 75);
        assert_eq!(lab.earned_points(), 20);
        assert_eq!(lab.completed_count(), 1);
    }
}
