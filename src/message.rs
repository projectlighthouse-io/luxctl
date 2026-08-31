use colored::Colorize;
use termimad::MadSkin;

use crate::api::{PaginatedResponse, Project, Task, TaskStatus};
use crate::state::ActiveProject;
use crate::tasks::{TestCase, TestResults};

// status symbols for consistent output (matching ui.rs)
const SYM_PASS: &str = "✓";
const SYM_FAIL: &str = "✗";
const SYM_PENDING: &str = "○";

pub struct Message;

impl Message {
    pub fn greet(name: &str) {
        let msg = format!(
            "hello {}, welcome to {}!",
            name.bold(),
            "projectlighthouse".yellow()
        );
        println!("{}", msg);
    }

    pub fn say(msg: &str) {
        println!("{}", msg);
    }

    pub fn cheer(msg: &str) {
        println!("{}", msg.green());
    }

    pub fn complain(msg: &str) {
        eprintln!("{}", msg.yellow());
    }

    pub fn oops(msg: &str) {
        eprintln!("{}", msg.red());
    }

    pub fn print_projects(response: &PaginatedResponse<Project>) {
        println!();
        for (i, project) in response.items.iter().enumerate() {
            Self::print_project(project, i);
        }
    }

    fn print_project(project: &Project, index: usize) {
        println!(
            "{}. {} {}",
            index,
            project.name.bold(),
            format!("/{}", project.slug).dimmed()
        );
        if let Some(desc) = &project.short_description {
            println!("{}", desc);
        }
        println!();
        println!();
    }

    pub fn print_project_detail(project: &Project) {
        println!("  {} {}", "#".dimmed(), project.name.bold());

        if let Some(desc) = &project.short_description {
            println!("    {}", desc);
        }

        println!("    {} {}", "slug:".dimmed(), project.slug.dimmed());
        println!("    {} {}", "url:".dimmed(), project.url().dimmed());

        println!();

        if let Some(tasks) = &project.tasks {
            println!("  {} ({}):\n", "tasks".bold(), tasks.len());

            let task_count = tasks.len();
            for (index, task) in tasks.iter().enumerate() {
                let is_last = index == task_count - 1;

                let connector = if is_last { "└" } else { "├" };
                let line_char = if is_last { " " } else { "│" };

                let status_marker = match task.status() {
                    TaskStatus::ChallengeCompleted => format!(" {}", SYM_PASS).green().to_string(),
                    TaskStatus::ChallengeFailed => format!(" {}", SYM_FAIL).red().to_string(),
                    _ => String::new(),
                };

                println!(
                    "    {}── {} {}  {}",
                    connector.dimmed(),
                    task.title.bold(),
                    task.scores().dimmed(),
                    status_marker
                );

                if !is_last {
                    println!("    {}", line_char.dimmed());
                }
            }

            if let Some(first_task) = tasks.first() {
                println!();
                println!("  {} {}", "next up:".dimmed(), first_task.title.bold());
                println!();

                // a project listing carries no briefs; `task show` fetches one.
                if !first_task.description().is_empty() {
                    let skin = MadSkin::default();
                    let rendered = format!("{}", skin.text(first_task.description(), None));
                    for line in rendered.lines() {
                        println!("    {}", line);
                    }
                }
            }
        }
    }

    pub fn print_task_header(task: &Task, detailed: bool) {
        println!("{}", task.title.bold());

        if detailed {
            let skin = MadSkin::default();
            let rendered = format!("{}", skin.text(task.description(), None));
            for line in rendered.lines() {
                println!("    {}", line);
            }
        }
    }

    pub fn print_task_detail(task: &Task, detailed: bool) {
        println!("{}", task.title.bold());

        if detailed {
            println!();
            let skin = MadSkin::default();
            let rendered = format!("{}", skin.text(task.description(), None));
            for line in rendered.lines() {
                println!("  {}", line);
            }
        }
    }

    pub fn print_validators_start(_count: usize) {
        println!("validating...");
    }

    pub fn print_test_case(test: &TestCase, index: usize) {
        if test.passed() {
            println!("{} #{} {}", SYM_PASS.green(), index + 1, test.name);
        } else {
            println!("{} #{} {}", SYM_FAIL.red(), index + 1, test.name.red());

            if test.message() != test.name {
                // truncate long error messages for display
                let msg = test.message();
                let display_msg = if msg.len() > 600 {
                    format!("{}...", &msg[..600])
                } else {
                    msg.to_string()
                };
                println!("  {}", display_msg.red());
            }
        }
    }

    pub fn print_test_results(results: &TestResults) {
        if results.all_passed() {
            println!(
                "{}",
                format!("all {} tests passed!", results.total()).green()
            );
        } else {
            println!(
                "{}",
                format!("{}/{} tests passed", results.passed(), results.total()).red()
            );
        }
    }

    pub fn print_connection_error(port: u16) {
        Self::oops(&format!("could not connect to server on port {}", port));
        println!("  make sure your server is running:");
        println!("  {}", "./your-server".dimmed());
    }

    pub fn print_task_list(project: &ActiveProject) {
        println!("tasks for: {}\n", project.name.bold());

        println!(
            "{}  {}  {}  {}",
            "#".dimmed(),
            format!("{:>6}", "Points").dimmed(),
            "Status".dimmed(),
            "Task".dimmed()
        );

        for (i, task) in project.tasks.iter().enumerate() {
            let (status, status_color) = match task.status {
                TaskStatus::ChallengeCompleted => (SYM_PASS.to_string(), "green"),
                TaskStatus::ChallengeFailed => (SYM_FAIL.to_string(), "red"),
                TaskStatus::Challenged => (SYM_PENDING.to_string(), "yellow"),
                TaskStatus::ChallengeAwaits | TaskStatus::ChallengeAbandoned => {
                    (SYM_PENDING.to_string(), "white")
                }
            };

            let status_display = match status_color {
                "green" => format!("  {}   ", status).green().to_string(),
                "red" => format!("  {}   ", status).red().to_string(),
                "yellow" => format!("  {}   ", status).yellow().to_string(),
                _ => format!("  {}   ", status).dimmed().to_string(),
            };

            let index = format!("{:02}", i + 1);
            let points = format!("{:>6}", task.points);
            println!(
                "{}  {}  {}  {}",
                index.dimmed(),
                points.bold(),
                status_display,
                task.title
            );
        }

        println!();
        println!(
            "progress: {}/{} completed | {} XP earned",
            project.completed_count().to_string().bold(),
            project.tasks.len(),
            format!("{}/{}", project.earned_points(), project.total_points()).bold()
        );
    }

    pub fn print_points_earned(points: i32) {
        if points > 0 {
            println!("{}", format!("+{} XP", points).bold().green());
        }
    }
}

/// Welcome/greet the user
#[macro_export]
macro_rules! greet {
    ($name:expr) => {
        $crate::message::Message::greet($name)
    };
}

/// General info message
#[macro_export]
macro_rules! say {
    ($msg:expr) => {
        $crate::message::Message::say($msg)
    };
    ($fmt:expr, $($arg:tt)*) => {
        $crate::message::Message::say(&format!($fmt, $($arg)*))
    };
}

/// Success message
#[macro_export]
macro_rules! cheer {
    ($msg:expr) => {
        $crate::message::Message::cheer($msg)
    };
    ($fmt:expr, $($arg:tt)*) => {
        $crate::message::Message::cheer(&format!($fmt, $($arg)*))
    };
}

/// Warning message
#[macro_export]
macro_rules! complain {
    ($msg:expr) => {
        $crate::message::Message::complain($msg)
    };
    ($fmt:expr, $($arg:tt)*) => {
        $crate::message::Message::complain(&format!($fmt, $($arg)*))
    };
}

/// Error message
#[macro_export]
macro_rules! oops {
    ($msg:expr) => {
        $crate::message::Message::oops($msg)
    };
    ($fmt:expr, $($arg:tt)*) => {
        $crate::message::Message::oops(&format!($fmt, $($arg)*))
    };
}
