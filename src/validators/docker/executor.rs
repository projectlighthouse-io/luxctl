//! Docker executor - runs containers from registered images only
//!
//! for security, only images registered in the registry module can be executed.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use super::registry::{self, ImageSource};

const DOCKERFILE_BASE_URL: &str =
    "https://raw.githubusercontent.com/projectlighthouse-io/luxctl/master/docker";
const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// result from running a container
#[derive(Debug)]
pub struct ExecutorResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl ExecutorResult {
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }
}

/// executor for running Dockerfiles
pub struct DockerExecutor {
    cache_dir: PathBuf,
}

impl DockerExecutor {
    pub fn new() -> Result<Self, String> {
        let cache_dir = dirs::home_dir()
            .ok_or("could not determine home directory")?
            .join(".luxctl")
            .join("docker_cache");

        std::fs::create_dir_all(&cache_dir)
            .map_err(|e| format!("failed to create cache dir: {}", e))?;

        Ok(Self { cache_dir })
    }

    /// download a Dockerfile by name from GitHub
    pub async fn download_dockerfile(&self, name: &str) -> Result<PathBuf, String> {
        let url = format!("{}/{}", DOCKERFILE_BASE_URL, name);
        let cache_path = self.cache_dir.join(name);

        // fetch from GitHub
        let response = reqwest::get(&url)
            .await
            .map_err(|e| format!("failed to fetch Dockerfile '{}': {}", name, e))?;

        if !response.status().is_success() {
            return Err(format!(
                "Dockerfile '{}' not found (status {})",
                name,
                response.status()
            ));
        }

        let content = response
            .text()
            .await
            .map_err(|e| format!("failed to read Dockerfile content: {}", e))?;

        // cache locally
        std::fs::write(&cache_path, &content)
            .map_err(|e| format!("failed to cache Dockerfile: {}", e))?;

        Ok(cache_path)
    }

    /// build and run a container from a registered image
    /// rejects unregistered images for security
    pub async fn run(
        &self,
        image_key: &str,
        workspace: &str,
        timeout_secs: Option<u64>,
    ) -> Result<ExecutorResult, String> {
        // security check: only allow registered images
        let registered = registry::lookup(image_key).ok_or_else(|| {
            format!(
                "image '{}' not registered. available: {:?}",
                image_key,
                registry::list_keys()
            )
        })?;

        // check docker availability
        if !is_docker_available().await {
            return Err("docker not available".to_string());
        }

        // handle based on image source type
        let dockerfile_path = match registered.source {
            ImageSource::Local(path) => {
                // download from GitHub (local means bundled in luxctl repo)
                self.download_dockerfile(path).await?
            }
            ImageSource::Remote(image_url) => {
                // for remote images, pull and run directly
                return self
                    .run_remote_image(image_url, workspace, timeout_secs)
                    .await;
            }
        };

        // resolve workspace to absolute path
        let workspace_path = std::fs::canonicalize(workspace)
            .map_err(|e| format!("cannot resolve workspace '{}': {}", workspace, e))?;

        let workspace_str = workspace_path.to_string_lossy();

        // generate unique image tag (sanitize to valid docker tag characters)
        let image_tag = format!(
            "luxctl-{}:{}",
            sanitize_for_docker_tag(image_key),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        );

        // build the image
        eprintln!("  building {} (this may take a moment)...", image_key);
        let build_result = self
            .docker_build(&dockerfile_path, &workspace_str, &image_tag)
            .await?;

        if !build_result.success() {
            return Ok(build_result);
        }

        // run the container
        eprintln!("  running validation...");
        let run_result = self
            .docker_run(&image_tag, &workspace_str, timeout_secs)
            .await;

        // cleanup: remove the image
        let _ = Command::new("docker")
            .args(["rmi", "-f", &image_tag])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;

        run_result
    }

    /// run a pre-built remote image (pulled from registry)
    async fn run_remote_image(
        &self,
        image_url: &str,
        workspace: &str,
        timeout_secs: Option<u64>,
    ) -> Result<ExecutorResult, String> {
        // resolve workspace to absolute path
        let workspace_path = std::fs::canonicalize(workspace)
            .map_err(|e| format!("cannot resolve workspace '{}': {}", workspace, e))?;

        let workspace_str = workspace_path.to_string_lossy();

        // pull the image
        eprintln!("  pulling {} ...", image_url);
        let pull_result = Command::new("docker")
            .args(["pull", image_url])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| format!("failed to pull image: {}", e))?;

        if !pull_result.status.success() {
            return Ok(ExecutorResult {
                exit_code: pull_result.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&pull_result.stdout).to_string(),
                stderr: String::from_utf8_lossy(&pull_result.stderr).to_string(),
            });
        }

        // run the container
        eprintln!("  running validation...");
        self.docker_run(image_url, &workspace_str, timeout_secs)
            .await
    }

    async fn docker_build(
        &self,
        dockerfile_path: &Path,
        context: &str,
        tag: &str,
    ) -> Result<ExecutorResult, String> {
        let output = Command::new("docker")
            .args([
                "build",
                "-f",
                dockerfile_path.to_string_lossy().as_ref(),
                "-t",
                tag,
                context,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| format!("failed to run docker build: {}", e))?;

        Ok(ExecutorResult {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }

    async fn docker_run(
        &self,
        image: &str,
        workspace: &str,
        timeout_secs: Option<u64>,
    ) -> Result<ExecutorResult, String> {
        let timeout_duration = Duration::from_secs(timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS));

        let result = timeout(
            timeout_duration,
            Command::new("docker")
                .args([
                    "run",
                    "--rm",
                    "--network=host",
                    "-v",
                    &format!("{}:/app", workspace),
                    "-w",
                    "/app",
                    image,
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output(),
        )
        .await;

        match result {
            Ok(Ok(output)) => Ok(ExecutorResult {
                exit_code: output.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            }),
            Ok(Err(e)) => Err(format!("docker run failed: {}", e)),
            Err(_) => Err(format!(
                "container timed out after {}s",
                timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS)
            )),
        }
    }
}

/// sanitize a string to be valid in a docker image tag
/// docker tags can only contain lowercase letters, digits, underscores, periods, and hyphens
fn sanitize_for_docker_tag(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' | '_' | '-' => c,
            _ => '-',
        })
        .collect()
}

/// check if docker is available
pub async fn is_docker_available() -> bool {
    Command::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_executor_result_success() {
        let result = ExecutorResult {
            exit_code: 0,
            stdout: "ok".to_string(),
            stderr: String::new(),
        };
        assert!(result.success());
    }

    #[test]
    fn test_executor_result_failure() {
        let result = ExecutorResult {
            exit_code: 1,
            stdout: String::new(),
            stderr: "error".to_string(),
        };
        assert!(!result.success());
    }

    #[tokio::test]
    async fn test_is_docker_available_returns_bool() {
        // just verify it doesn't panic
        let _ = is_docker_available().await;
    }

    #[tokio::test]
    async fn test_run_rejects_unregistered_image() {
        let executor = DockerExecutor::new().unwrap();
        let result = executor.run("malicious-image", ".", None).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("not registered"));
        assert!(err.contains("malicious-image"));
    }

    #[tokio::test]
    async fn test_run_rejects_arbitrary_url() {
        let executor = DockerExecutor::new().unwrap();
        // even if it looks like a valid image URL, it must be registered
        let result = executor.run("ghcr.io/evil/malware:latest", ".", None).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("not registered"));
    }

    #[test]
    fn test_registered_images_are_known() {
        // verify our test images are actually registered
        assert!(registry::is_registered("go1.22"));
        assert!(registry::is_registered("go1.22-race"));
        assert!(registry::is_registered("api-client-test"));
    }

    #[test]
    fn test_sanitize_for_docker_tag() {
        assert_eq!(sanitize_for_docker_tag("go1.22"), "go1-22");
        assert_eq!(sanitize_for_docker_tag("Go1.22"), "go1-22");
        assert_eq!(sanitize_for_docker_tag("foo:bar"), "foo-bar");
        assert_eq!(sanitize_for_docker_tag("foo/bar"), "foo-bar");
        assert_eq!(
            sanitize_for_docker_tag("api-client-test"),
            "api-client-test"
        );
        assert_eq!(sanitize_for_docker_tag("test_image"), "test_image");
    }
}
