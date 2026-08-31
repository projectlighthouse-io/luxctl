use clap::{CommandFactory, Parser, Subcommand, ValueHint};
use clap_complete::{generate, Shell};
use color_eyre::eyre::Result;

use luxctl::{
    api::LighthouseAPIClient, auth::TokenAuthenticator, commands, config::Config, greet,
    message::Message, oops, LIGHTHOUSE_URL, VERSION,
};

#[derive(Parser)]
#[command(name = "luxctl")]
#[command(version = VERSION)]
struct Cli {
    #[command(subcommand)]
    commands: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Log in with your API token from projectlighthouse.io
    Auth {
        #[arg(short = 't', long)]
        token: String,
    },

    /// See your profile and progress
    Whoami,

    /// Projects are a series of challenges that build on each other, preparing you for real-world problems
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },

    /// Tasks are individual challenges within a project - tackle them in any order
    Task {
        #[command(subcommand)]
        action: TaskAction,
    },

    /// Test your solution to see if it passes
    Run {
        #[arg(short = 'p', long)]
        project: Option<String>,

        #[arg(short = 't', long)]
        task: String,

        #[arg(short = 'd', long)]
        detailed: bool,
    },

    /// Run all the tasks of a project at once
    Validate {
        #[arg(short = 'd', long)]
        detailed: bool,

        #[arg(short = 'a', long)]
        all: bool,
    },

    /// Submit answers for blueprint tasks that require user input
    Result {
        #[arg(short = 'p', long)]
        project: Option<String>,

        #[arg(short = 't', long)]
        task: String,

        /// Input values as key=value pairs (repeatable)
        #[arg(short = 'i', long = "input")]
        inputs: Vec<String>,
    },

    /// Parse a .bp file and print the transpiled IR as JSON (offline, no auth)
    Export {
        /// Path to the .bp file
        #[arg(value_hint = ValueHint::FilePath)]
        file: String,

        /// Output format
        #[arg(short = 'f', long, default_value = "json")]
        format: String,
    },

    /// Stuck on a task? Hints can help, but they might cost you XP
    Hint {
        #[command(subcommand)]
        action: HintAction,
    },

    /// Manage the pinned Linux environment that `linux|` blueprints run in
    Env {
        #[command(subcommand)]
        action: EnvAction,
    },

    /// Download or refresh fixture files for the active project
    Sync,

    /// Upgrade luxctl to the latest version (or a specific version)
    Upgrade {
        /// Target version (e.g. v0.9.2). Defaults to latest release.
        version: Option<String>,
    },

    /// Check your environment and diagnose issues
    Doctor,

    /// Print the current version
    Version,

    /// Generate shell completions for luxctl
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: Shell,
    },

    /// Project-specific helper tools (e.g., data generators)
    Helper {
        /// Helper name (e.g., 1brc)
        name: String,

        /// Number of rows to generate
        #[arg(short = 'r', long, default_value = "10000")]
        rows: u64,

        /// Measurements output file
        #[arg(short = 'm', long, default_value = "data/measurements.txt", value_hint = ValueHint::FilePath)]
        measurements: String,

        /// Expected output file
        #[arg(short = 'e', long, default_value = "expected/output.txt", value_hint = ValueHint::FilePath)]
        expected: String,
    },
}

#[derive(Subcommand)]
enum ProjectAction {
    /// See all available projects you can work on
    List {
        /// Open in browser: no value opens /projects, with index opens that project
        #[arg(short = 'w', long, num_args = 0..=1, default_missing_value = "-1")]
        web: Option<i32>,
    },
    /// Get details about a project before starting
    Show {
        #[arg(short = 's', long)]
        slug: String,
    },
    /// Begin working on a project in your current directory
    Start {
        /// Project slug or index from `project list`
        #[arg(short = 'i', long)]
        id: String,

        /// Workspace directory (defaults to current directory)
        #[arg(short = 'w', long, default_value = ".", value_hint = ValueHint::DirPath)]
        workspace: String,

        /// Runtime environment (go, rust, c)
        #[arg(short = 'r', long)]
        runtime: Option<String>,
    },
    /// See your progress on the current project
    Status,
    /// Stop working on the current project
    Stop,
    /// Change project settings (runtime, workspace)
    Set {
        /// Runtime environment (go, rust, c)
        #[arg(short = 'r', long)]
        runtime: Option<String>,

        /// Workspace directory
        #[arg(short = 'w', long, value_hint = ValueHint::DirPath)]
        workspace: Option<String>,
    },
    /// Start fresh - reset all progress on the current project
    Restart,
}

#[derive(Subcommand)]
enum TaskAction {
    /// See all tasks in your current project
    List {
        #[arg(short = 'r', long)]
        refresh: bool,
    },
    /// Read the task description and requirements
    Show {
        /// Task number or slug
        #[arg(short = 't', long)]
        task: String,

        /// Show full description
        #[arg(short = 'd', long)]
        detailed: bool,
    },
}

#[derive(Subcommand)]
enum HintAction {
    /// See what hints are available for a task
    List {
        #[arg(short = 't', long)]
        task: String,
    },
    /// Reveal a hint - this might cost you XP
    Unlock {
        #[arg(short = 't', long)]
        task: String,

        #[arg(short = 'i', long)]
        hint: String,
    },
}

#[derive(Subcommand)]
enum EnvAction {
    /// See the detected backend and what it provides
    Status,
    /// Provision and start the pinned Lighthouse environment
    Up,
    /// Stop the pinned environment
    Down,
    /// Pull the current pinned image into the active backend
    Upgrade,
}

impl Commands {
    pub const AUTH_USAGE: &'static str = "luxctl auth --token $token";
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    env_logger::init();

    let cli = Cli::parse();

    match cli.commands {
        Commands::Auth { token } => {
            let authenticator = TokenAuthenticator::new(&token);

            match authenticator.authenticate().await {
                Ok(user) => {
                    greet!(user.name());
                }
                Err(err) => {
                    log::error!("{}", err);
                    oops!("{}", err);
                }
            }
        }

        Commands::Whoami => {
            let config = match Config::load() {
                Ok(c) if c.has_auth_token() => c,
                _ => {
                    println!("nobody");
                    println!("login with: {}", Commands::AUTH_USAGE);
                    return Ok(());
                }
            };

            let client = LighthouseAPIClient::from_config(&config);
            match client.me().await {
                Ok(user) => println!("{}", user.name),
                Err(err) => oops!("failed to fetch user: {}", err),
            }
        }

        Commands::Project { action } => match action {
            ProjectAction::List { web } => {
                let config = Config::load()?;
                if !config.has_auth_token() {
                    oops!("not authenticated. Run: `{}`", Commands::AUTH_USAGE);
                    return Ok(());
                }

                let client = LighthouseAPIClient::from_config(&config);
                match client.projects(None, None).await {
                    Ok(response) => {
                        Message::print_projects(&response);
                        if let Some(index) = web {
                            let url = if index < 0 {
                                format!("{}/projects", LIGHTHOUSE_URL)
                            } else if let Some(project) = response.items.get(index as usize) {
                                project.url()
                            } else {
                                oops!("invalid index: {}", index);
                                return Ok(());
                            };
                            let _ = std::process::Command::new("open").arg(&url).spawn();
                        }
                    }
                    Err(err) => {
                        oops!("failed to fetch projects: {}", err);
                    }
                }
            }
            ProjectAction::Show { slug } => {
                let config = Config::load()?;
                if !config.has_auth_token() {
                    oops!("not authenticated. Run: `{}`", Commands::AUTH_USAGE);
                    return Ok(());
                }

                let client = LighthouseAPIClient::from_config(&config);
                match client.project_by_slug(&slug).await {
                    Ok(project) => {
                        Message::print_project_detail(&project);
                    }
                    Err(err) => {
                        oops!("failed to fetch project: {}", err);
                    }
                }
            }
            ProjectAction::Start {
                id,
                workspace,
                runtime,
            } => {
                let actual_slug = if let Ok(index) = id.parse::<usize>() {
                    let config = Config::load()?;
                    if !config.has_auth_token() {
                        oops!("not authenticated. Run: `{}`", Commands::AUTH_USAGE);
                        return Ok(());
                    }
                    let client = LighthouseAPIClient::from_config(&config);
                    match client.projects(None, None).await {
                        Ok(response) => {
                            if let Some(project) = response.items.get(index) {
                                project.slug.clone()
                            } else {
                                oops!("invalid index: {}", index);
                                return Ok(());
                            }
                        }
                        Err(err) => {
                            oops!("failed to fetch projects: {}", err);
                            return Ok(());
                        }
                    }
                } else {
                    id
                };
                commands::project::start(&actual_slug, &workspace, runtime.as_deref()).await?;
            }
            ProjectAction::Status => {
                commands::project::status()?;
            }
            ProjectAction::Stop => {
                commands::project::stop()?;
            }
            ProjectAction::Set { runtime, workspace } => {
                if let Some(ref rt) = runtime {
                    commands::project::set_runtime(rt)?;
                }
                if let Some(ref ws) = workspace {
                    commands::project::set_workspace(ws)?;
                }
                if runtime.is_none() && workspace.is_none() {
                    oops!("provide --runtime or --workspace to set");
                }
            }
            ProjectAction::Restart => {
                commands::project::restart().await?;
            }
        },

        Commands::Task { action } => match action {
            TaskAction::List { refresh } => {
                commands::tasks::list(refresh).await?;
            }
            TaskAction::Show { task, detailed } => {
                commands::task::show(&task, detailed).await?;
            }
        },

        Commands::Env { action } => match action {
            EnvAction::Status => commands::env::status().await?,
            EnvAction::Up => commands::env::up().await?,
            EnvAction::Down => commands::env::down().await?,
            EnvAction::Upgrade => commands::env::upgrade().await?,
        },

        Commands::Run {
            project,
            task,
            detailed,
        } => {
            commands::run::run(&task, project.as_deref(), detailed).await?;
        }

        Commands::Validate { detailed, all } => {
            commands::validate::validate_all(all, detailed).await?;
        }

        Commands::Result {
            project,
            task,
            inputs,
        } => {
            commands::result::result(&task, &inputs, project.as_deref()).await?;
        }

        Commands::Export { file, format } => {
            commands::export::export(&file, &format)?;
        }

        Commands::Hint { action } => match action {
            HintAction::List { task } => {
                commands::hints::list(&task).await?;
            }
            HintAction::Unlock { task, hint } => {
                commands::hints::unlock(&task, &hint).await?;
            }
        },

        Commands::Sync => {
            commands::sync::run().await?;
        }

        Commands::Upgrade { version } => {
            commands::upgrade::run(version).await?;
        }

        Commands::Doctor => {
            commands::doctor::run().await?;
        }

        Commands::Version => {
            println!("luxctl v{VERSION}");
        }

        Commands::Completions { shell } => {
            generate(shell, &mut Cli::command(), "luxctl", &mut std::io::stdout());
        }

        Commands::Helper {
            name,
            rows,
            measurements,
            expected,
        } => {
            commands::helpers::run(&name, rows, &measurements, &expected)?;
        }
    }

    Ok(())
}
