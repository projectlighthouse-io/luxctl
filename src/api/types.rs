use serde::{Deserialize, Serialize};

use crate::LIGHTHOUSE_URL;

/// a refusal from the api.
///
/// `code` is stable wire text and safe to branch on; `error` is english prose
/// and only a fallback. see the api's `projects/refusal.rs`.
#[derive(Debug, Deserialize)]
pub struct ApiError {
    #[serde(default)]
    pub code: String,
    pub error: String,
}

#[derive(Debug, Deserialize)]
pub struct HealthCheckResponse {
    pub status: String,
    pub app: Option<String>,
    pub version: Option<String>,
}

/// a page of results.
///
/// four fields, and no `links`: the api never built absolute urls for a client
/// that already knows its own base url, and every caller here pages by number.
#[derive(Debug, Deserialize)]
pub struct PaginatedResponse<T> {
    pub items: Vec<T>,
    pub page: i64,
    pub per_page: i64,
    pub total: i64,
}

#[derive(Debug, Deserialize)]
pub struct ApiUser {
    pub id: i64,
    pub name: String,
    pub email: String,
    #[serde(default)]
    pub stats: Option<UserStats>,
}

#[derive(Debug, Deserialize)]
pub struct UserStats {
    pub projects_attempted: i64,
    pub tasks_completed: i64,
    pub total_xp: i64,
}

/// how a project's tasks unlock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UnlockMode {
    /// every task is available from the start.
    #[default]
    Open,
    /// a task opens when the one before it is complete.
    Sequential,
}

/// a bullet point beside the project's pitch.
#[derive(Debug, Deserialize)]
pub struct Feature {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Project {
    /// a uuid the content repo mints. the integer primary keys are gone.
    pub id: String,
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub headline: Option<String>,
    #[serde(default)]
    pub short_description: Option<String>,
    #[serde(default)]
    pub long_description: Option<String>,
    #[serde(default)]
    pub difficulty: Option<String>,
    #[serde(default)]
    pub runner_image: Option<String>,
    #[serde(default)]
    pub is_challenge: bool,
    #[serde(default)]
    pub is_featured: bool,
    #[serde(default)]
    pub featured_order: i16,
    #[serde(default)]
    pub show_tasks: bool,
    #[serde(default)]
    pub unlock_mode: UnlockMode,
    #[serde(default)]
    pub related_book_slug: Option<String>,
    #[serde(default)]
    pub task_count: usize,
    #[serde(default)]
    pub features: Vec<Feature>,
    /// the project's long-form markdown. only on the detail response.
    #[serde(default)]
    pub overview: Option<String>,
    /// the `.bp` source, whole.
    ///
    /// **one per project, not one per task.** the old api stored a copy of
    /// this on every task row and served it there; it is read once here, and
    /// every task of the project runs against it.
    #[serde(default)]
    pub blueprint: Option<String>,
    #[serde(default)]
    pub tasks: Option<Vec<Task>>,
}

impl Project {
    pub fn url(&self) -> String {
        format!("{}/projects/{}", LIGHTHOUSE_URL, self.slug)
    }

    /// the blueprint source, or an empty string when the project carries none.
    pub fn blueprint_source(&self) -> &str {
        self.blueprint.as_deref().unwrap_or_default()
    }

    pub fn has_blueprint(&self) -> bool {
        self.blueprint.as_ref().is_some_and(|s| !s.is_empty())
    }
}

/// task input type — whether the task expects a typed answer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskInputType {
    #[default]
    None,
    Text,
}

/// where a reader stands on a task.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    ChallengeAwaits,
    Challenged,
    ChallengeCompleted,
    ChallengeFailed,
    ChallengeAbandoned,
}

impl TaskStatus {
    pub fn is_completed(self) -> bool {
        self == TaskStatus::ChallengeCompleted
    }
}

/// what this reader has done with one task.
///
/// absent for an anonymous caller, which is a different statement from a row
/// of zeroes: signed out means "no progress", not "no attempts". everything
/// reads it through [`Task`]'s accessors, which default it.
#[derive(Debug, Default, Deserialize)]
pub struct TaskProgress {
    #[serde(default)]
    pub status: TaskStatus,
    #[serde(default)]
    pub attempts: i64,
    #[serde(default)]
    pub points_earned: i32,
    /// sequential lock: the task before this one is not done.
    #[serde(default)]
    pub is_locked: bool,
    /// paywall: the task is not free and the reader does not hold the book.
    #[serde(default)]
    pub is_paid: bool,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Task {
    /// a uuid, minted in the content repo.
    pub id: String,
    pub slug: String,
    pub title: String,
    /// the brief, as markdown. absent in a listing, and absent for a phase
    /// that carried no description.
    #[serde(default)]
    pub description: Option<String>,
    pub sort_order: i32,
    #[serde(default)]
    pub input_type: TaskInputType,
    /// the scoring ladder, verbatim — `5:10:25|10:20:15`.
    #[serde(default)]
    pub scores: Option<String>,
    #[serde(default)]
    pub points: i32,
    #[serde(default)]
    pub is_free: bool,
    #[serde(default)]
    pub abandoned_deduction: i32,
    #[serde(default)]
    pub hints: Vec<Hint>,
    /// absent for an anonymous caller.
    #[serde(default)]
    pub progress: Option<TaskProgress>,
    /// commands run before and after the blueprint. the api has no field for
    /// either any more; kept so a response that grows one still parses.
    #[serde(default)]
    pub prologue: Vec<String>,
    #[serde(default)]
    pub epilogue: Vec<String>,
}

impl Task {
    /// check if this task accepts user input
    pub fn accepts_input(&self) -> bool {
        self.input_type != TaskInputType::None
    }

    pub fn description(&self) -> &str {
        self.description.as_deref().unwrap_or_default()
    }

    pub fn scores(&self) -> &str {
        self.scores.as_deref().unwrap_or_default()
    }

    /// this reader's standing, or the untouched one for a signed-out caller.
    pub fn status(&self) -> TaskStatus {
        self.progress
            .as_ref()
            .map_or(TaskStatus::ChallengeAwaits, |p| p.status)
    }

    pub fn points_earned(&self) -> i32 {
        self.progress.as_ref().map_or(0, |p| p.points_earned)
    }

    /// both locks are false without progress: a signed-out reader is not told
    /// a task is locked, because the api has not decided that it is.
    pub fn is_locked(&self) -> bool {
        self.progress.as_ref().is_some_and(|p| p.is_locked)
    }

    pub fn is_paid(&self) -> bool {
        self.progress.as_ref().is_some_and(|p| p.is_paid)
    }
}

/// a hint on a project or task listing: that one exists, what it costs, and
/// whether it is open yet. never its words.
#[derive(Debug, Deserialize)]
pub struct Hint {
    pub id: String,
    #[serde(default)]
    pub sort_order: usize,
    pub points_deduction: i32,
    #[serde(default)]
    pub is_available: bool,
}

/// a hint from the hints endpoint: the summary, plus what this reader holds.
///
/// `text` is absent rather than null when the hint is not theirs, so a client
/// cannot print an empty hint by forgetting to check a flag.
#[derive(Debug, Deserialize)]
pub struct TaskHint {
    pub id: String,
    #[serde(default)]
    pub sort_order: usize,
    pub points_deduction: i32,
    #[serde(default)]
    pub is_available: bool,
    #[serde(default)]
    pub is_unlocked: bool,
    #[serde(default)]
    pub text: Option<String>,
}

/// outcome values for task attempts
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutcome {
    Attempted,
    Passed,
    Failed,
    Abandoned,
}

impl std::fmt::Display for TaskOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskOutcome::Attempted => write!(f, "attempted"),
            TaskOutcome::Passed => write!(f, "passed"),
            TaskOutcome::Failed => write!(f, "failed"),
            TaskOutcome::Abandoned => write!(f, "abandoned"),
        }
    }
}

/// request body for submitting a task attempt.
///
/// no `points_achieved` and no `run`: the api works both out from its own log,
/// and ignores anything a client says about them.
#[derive(Debug, Serialize)]
pub struct SubmitAttemptRequest {
    pub project_slug: String,
    pub task_id: String,
    pub task_outcome: TaskOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_outcome_context: Option<String>,
}

/// what comes back from recording an attempt.
#[derive(Debug, Deserialize)]
pub struct AttemptData {
    pub id: i64,
    pub task_id: String,
    /// which run of the project this attempt was made in.
    pub run: i16,
    pub outcome: TaskOutcome,
    pub points_achieved: i32,
    pub is_reattempt: bool,
    #[serde(default)]
    pub created_at: Option<String>,
}

/// what comes back from restarting a project.
#[derive(Debug, Deserialize)]
pub struct RestartProjectData {
    /// the run the reader is now on. replaces the old `attempt_group_id`,
    /// which was a row id the client had no use for.
    pub run: i16,
    #[serde(default)]
    pub restarted_at: Option<String>,
}

impl ApiUser {
    pub fn id(&self) -> i64 {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_user_accessors() {
        let user = ApiUser {
            id: 42,
            name: "Test User".to_string(),
            email: "test@example.com".to_string(),
            stats: None,
        };

        assert_eq!(user.id(), 42);
        assert_eq!(user.name(), "Test User");
    }

    #[test]
    fn test_project_deserialize() {
        let json = r#"{
            "id": "8053a849-9423-4a89-8e90-b343bdaea79d",
            "slug": "build-your-own-http-server",
            "name": "Build Your Own Server",
            "headline": "Your First HTTP Server",
            "short_description": "Learn the fundamentals of web servers.",
            "difficulty": "intermediate",
            "runner_image": "local|go|rust|c",
            "is_challenge": false,
            "is_featured": true,
            "featured_order": 2,
            "show_tasks": true,
            "unlock_mode": "sequential",
            "related_book_slug": "build-your-own-http-server",
            "task_count": 18
        }"#;

        let project: Project = serde_json::from_str(json).unwrap();

        assert_eq!(project.id, "8053a849-9423-4a89-8e90-b343bdaea79d");
        assert_eq!(project.slug, "build-your-own-http-server");
        assert_eq!(project.headline.as_deref(), Some("Your First HTTP Server"));
        assert_eq!(project.unlock_mode, UnlockMode::Sequential);
        assert!(project.is_featured);
        assert!(project.show_tasks);
        assert_eq!(project.task_count, 18);
        // a listing carries neither
        assert!(project.blueprint.is_none());
        assert!(project.tasks.is_none());
    }

    #[test]
    fn test_project_detail_carries_the_blueprint_once() {
        let json = r#"{
            "id": "8053a849-9423-4a89-8e90-b343bdaea79d",
            "slug": "build-your-own-git",
            "name": "Build Your Own Git",
            "runner_image": "local|go|rust|c",
            "unlock_mode": "sequential",
            "task_count": 1,
            "features": [{"title": "Quick Start", "description": "Three tasks", "icon": "zap"}],
            "overview": "Start simple.\n",
            "blueprint": "phase \"listen\" { }",
            "tasks": [
                {
                    "id": "a9dccd78-7d42-476f-a9d1-83517426449f",
                    "slug": "initialize-a-repository",
                    "title": "Initialize a Repository",
                    "sort_order": 1,
                    "scores": "5:10:50|10:20:35",
                    "points": 50,
                    "is_free": true,
                    "abandoned_deduction": 5,
                    "hints": [
                        {
                            "id": "5e4ebe7d-909c-4d72-814b-7432864e178e",
                            "sort_order": 0,
                            "points_deduction": 5,
                            "is_available": false
                        }
                    ],
                    "progress": {
                        "status": "challenge_completed",
                        "attempts": 3,
                        "points_earned": 35,
                        "is_locked": false,
                        "is_paid": false,
                        "started_at": "2026-08-30T10:00:00Z",
                        "completed_at": "2026-08-30T10:12:00Z"
                    }
                }
            ]
        }"#;

        let project: Project = serde_json::from_str(json).unwrap();

        assert!(project.has_blueprint());
        assert_eq!(project.features.len(), 1);
        assert_eq!(project.overview.as_deref(), Some("Start simple.\n"));

        let tasks = project.tasks.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Initialize a Repository");
        assert_eq!(tasks[0].status(), TaskStatus::ChallengeCompleted);
        assert_eq!(tasks[0].points_earned(), 35);
        assert_eq!(tasks[0].hints.len(), 1);
        assert_eq!(tasks[0].hints[0].id, "5e4ebe7d-909c-4d72-814b-7432864e178e");
    }

    #[test]
    fn test_task_without_progress_reads_as_untouched() {
        // what an anonymous caller gets: no `progress` key at all.
        let json = r#"{
            "id": "a9dccd78-7d42-476f-a9d1-83517426449f",
            "slug": "listen-on-port",
            "title": "Listen on a Port",
            "sort_order": 1,
            "points": 25,
            "abandoned_deduction": 5
        }"#;

        let task: Task = serde_json::from_str(json).unwrap();

        assert_eq!(task.status(), TaskStatus::ChallengeAwaits);
        assert_eq!(task.points_earned(), 0);
        assert!(!task.is_locked());
        assert!(!task.is_paid());
        assert_eq!(task.scores(), "");
        assert_eq!(task.description(), "");
        assert!(!task.accepts_input());
    }

    #[test]
    fn test_paginated_response_deserialize() {
        let json = r#"{
            "items": [
                {
                    "id": "11111111-1111-1111-1111-111111111111",
                    "slug": "lab-one",
                    "name": "Lab One",
                    "short_description": "First lab",
                    "task_count": 5
                },
                {
                    "id": "22222222-2222-2222-2222-222222222222",
                    "slug": "lab-two",
                    "name": "Lab Two",
                    "short_description": "Second lab",
                    "task_count": 3
                }
            ],
            "page": 1,
            "per_page": 50,
            "total": 16
        }"#;

        let response: PaginatedResponse<Project> = serde_json::from_str(json).unwrap();

        assert_eq!(response.items.len(), 2);
        assert_eq!(response.items[0].slug, "lab-one");
        assert_eq!(response.items[1].slug, "lab-two");
        assert_eq!(response.page, 1);
        assert_eq!(response.per_page, 50);
        assert_eq!(response.total, 16);
    }

    #[test]
    fn test_paginated_response_empty_items() {
        let json = r#"{"items": [], "page": 1, "per_page": 50, "total": 0}"#;

        let response: PaginatedResponse<Project> = serde_json::from_str(json).unwrap();

        assert!(response.items.is_empty());
        assert_eq!(response.total, 0);
    }

    #[test]
    fn test_attempt_data_deserialize() {
        let json = r#"{
            "id": 91,
            "task_id": "a9dccd78-7d42-476f-a9d1-83517426449f",
            "run": 4,
            "outcome": "passed",
            "points_achieved": 100,
            "is_reattempt": false,
            "created_at": "2026-08-30T10:12:00Z"
        }"#;

        let data: AttemptData = serde_json::from_str(json).unwrap();

        assert_eq!(data.run, 4);
        assert_eq!(data.outcome, TaskOutcome::Passed);
        assert_eq!(data.points_achieved, 100);
        assert!(!data.is_reattempt);
    }

    #[test]
    fn test_restart_answers_a_run_not_a_group_id() {
        let json = r#"{"run": 3, "restarted_at": "2026-08-30T10:12:00Z"}"#;

        let data: RestartProjectData = serde_json::from_str(json).unwrap();

        assert_eq!(data.run, 3);
    }

    #[test]
    fn test_submit_attempt_request_carries_no_points_or_run() {
        let request = SubmitAttemptRequest {
            project_slug: "p".to_string(),
            task_id: "a9dccd78-7d42-476f-a9d1-83517426449f".to_string(),
            task_outcome: TaskOutcome::Passed,
            task_outcome_context: None,
        };

        let json = serde_json::to_string(&request).unwrap();

        assert!(json.contains("\"task_outcome\":\"passed\""));
        assert!(json.contains("a9dccd78-7d42-476f-a9d1-83517426449f"));
        assert!(!json.contains("points_achieved"));
        assert!(!json.contains("\"run\""));
        // an absent context is dropped rather than sent as null
        assert!(!json.contains("task_outcome_context"));
    }

    #[test]
    fn test_locked_hint_carries_no_text() {
        let json = r#"{
            "id": "5e4ebe7d-909c-4d72-814b-7432864e178e",
            "sort_order": 0,
            "points_deduction": 5,
            "is_available": true,
            "is_unlocked": false
        }"#;

        let hint: TaskHint = serde_json::from_str(json).unwrap();

        assert!(hint.is_available);
        assert!(!hint.is_unlocked);
        assert!(hint.text.is_none());
    }

    #[test]
    fn test_api_error_reads_the_code_and_the_prose() {
        let json = r#"{"code": "task_locked", "error": "Finish the task before this one first."}"#;

        let error: ApiError = serde_json::from_str(json).unwrap();

        assert_eq!(error.code, "task_locked");
        assert_eq!(error.error, "Finish the task before this one first.");
    }
}
