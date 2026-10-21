//! Docker executor - runs containers from registered images only
//!
//! for security, only images registered in the registry module can be executed.

use bollard::body_full;
use bollard::container::LogOutput;
use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
    BuildImageOptionsBuilder, BuilderVersion, CreateContainerOptions, CreateImageOptionsBuilder,
    KillContainerOptions, LogsOptionsBuilder, RemoveContainerOptionsBuilder, RemoveImageOptions,
    StartContainerOptions, WaitContainerOptions,
};
use bollard::Docker;
use futures_util::StreamExt;
use std::io::Write;
use std::path::{Path, PathBuf};
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
    docker: Docker,
}

impl DockerExecutor {
    pub fn new() -> Result<Self, String> {
        let cache_dir = dirs::home_dir()
            .ok_or("could not determine home directory")?
            .join(".luxctl")
            .join("docker_cache");

        std::fs::create_dir_all(&cache_dir)
            .map_err(|e| format!("failed to create cache dir: {}", e))?;

        // connect to Docker
        let docker = Docker::connect_with_local_defaults()
            .map_err(|e| format!("failed to connect to Docker daemon: {}", e))?;

        Ok(Self { cache_dir, docker })
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
        let _ = self.remove_image(&image_tag).await;

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
        let options = CreateImageOptionsBuilder::default()
            .from_image(image_url)
            .build();

        let mut pull_stream = self.docker.create_image(Some(options), None, None);

        while let Some(item) = pull_stream.next().await {
            item.map_err(|e| format!("failed to pull image: '{}': {}", image_url, e))?;
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
        let tar_gz = self
            .build_context_tarball(dockerfile_path, context)
            .map_err(|e| format!("failed to build context tarball: {}", e))?;

        let options = BuildImageOptionsBuilder::default()
            .dockerfile("Dockerfile")
            .t(tag)
            .rm(true)
            .version(BuilderVersion::BuilderBuildKit)
            .build();

        let mut build_stream =
            self.docker
                .build_image(options, None, Some(body_full(tar_gz.into())));

        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut had_error = false;

        while let Some(msg) = build_stream.next().await {
            match msg {
                Ok(info) => {
                    if let Some(stream) = info.stream {
                        stdout.push_str(&stream);
                    }

                    if let Some(err_detail) = info.error_detail {
                        if let Some(msg) = err_detail.message {
                            stderr.push_str(&msg);
                        }
                        had_error = true;
                    }
                }
                Err(e) => {
                    stderr.push_str(&format!("{}\n", e));
                    had_error = true;
                }
            }
        }

        Ok(ExecutorResult {
            exit_code: if had_error { 1 } else { 0 },
            stdout,
            stderr,
        })
    }

    async fn docker_run(
        &self,
        image: &str,
        workspace: &str,
        timeout_secs: Option<u64>,
    ) -> Result<ExecutorResult, String> {
        let secs = timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS);
        let config = ContainerCreateBody {
            image: Some(image.to_string()),
            working_dir: Some("/app".to_string()),
            host_config: Some(HostConfig {
                binds: Some(vec![format!("{}:/app", workspace)]),
                network_mode: Some("host".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let container = self
            .docker
            .create_container(None::<CreateContainerOptions>, config)
            .await
            .map_err(|e| format!("failed to create container: {}", e))?;

        let id = container.id;

        if let Err(e) = self
            .docker
            .start_container(&id, None::<StartContainerOptions>)
            .await
        {
            let _ = self.cleanup_container(&id).await;
            return Err(format!("failed to start container: {}", e));
        }

        let wait_stream = self
            .docker
            .wait_container(&id, None::<WaitContainerOptions>);

        let exit_code =
            match timeout(Duration::from_secs(secs), wait_stream.collect::<Vec<_>>()).await {
                Ok(results) => match results.into_iter().last() {
                    Some(Ok(res)) => res.status_code as i32,
                    Some(Err(e)) => {
                        let _ = self.cleanup_container(&id).await;
                        return Err(format!("failed to wait for container: {}", e));
                    }
                    None => -1,
                },

                Err(_) => {
                    let _ = self
                        .docker
                        .kill_container(&id, None::<KillContainerOptions>)
                        .await;
                    let _ = self.cleanup_container(&id).await;
                    return Err(format!("container timed out after {} seconds", secs));
                }
            };

        let options = LogsOptionsBuilder::default()
            .stderr(true)
            .stdout(true)
            .build();

        let mut log_stream = self.docker.logs(&id, Some(options));

        let mut stdout = String::new();
        let mut stderr = String::new();

        while let Some(chunk) = log_stream.next().await {
            match chunk {
                Ok(LogOutput::StdOut { message }) => {
                    stdout.push_str(&String::from_utf8_lossy(&message));
                }
                Ok(LogOutput::StdErr { message }) => {
                    stderr.push_str(&String::from_utf8_lossy(&message));
                }
                _ => {}
            }
        }

        let _ = self.cleanup_container(&id).await;

        Ok(ExecutorResult {
            exit_code,
            stdout,
            stderr,
        })
    }

    async fn cleanup_container(&self, id: &str) -> Result<(), String> {
        let options = RemoveContainerOptionsBuilder::default().force(true).build();
        self.docker
            .remove_container(id, Some(options))
            .await
            .map_err(|e| format!("failed to remove container '{}': {}", id, e))?;
        Ok(())
    }

    async fn remove_image(&self, tag: &str) -> Result<(), String> {
        self.docker
            .remove_image(tag, None::<RemoveImageOptions>, None)
            .await
            .map_err(|e| format!("failed to remove image '{}': {}", tag, e))?;
        Ok(())
    }

    pub async fn is_docker_available(&self) -> bool {
        self.docker.version().await.is_ok()
    }

    fn build_context_tarball(
        &self,
        dockerfile_path: &Path,
        context: &str,
    ) -> Result<Vec<u8>, String> {
        use flate2::write::GzEncoder;
        use flate2::Compression;

        let mut tar = tar::Builder::new(Vec::new());

        tar.append_dir_all("", context)
            .map_err(|e| format!("failed to create tarball: {}", e))?;

        let dockerfile_content = std::fs::read(dockerfile_path)
            .map_err(|e| format!("failed to read Dockerfile: {}", e))?;

        let mut header = tar::Header::new_gnu();
        header
            .set_path("Dockerfile")
            .map_err(|e| format!("failed to create tarball: {}", e))?;
        header.set_size(dockerfile_content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();

        tar.append(&header, dockerfile_content.as_slice())
            .map_err(|e| format!("failed to append Dockerfile to tar: {}", e))?;

        let uncompressed = tar
            .into_inner()
            .map_err(|e| format!("failed to finalize tar: {}", e))?;

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(&uncompressed)
            .map_err(|e| format!("failed to compress tarball: {}", e))?;

        encoder
            .finish()
            .map_err(|e| format!("failed to finish gzip: {}", e))
    }
}

/// check if docker is available
pub async fn is_docker_available() -> bool {
    match Docker::connect_with_local_defaults() {
        Ok(docker) => docker.version().await.is_ok(),
        Err(_) => false,
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

    #[test]
    fn test_tarball_contains_dockerfile() {
        use flate2::read::GzDecoder;
        use std::io::Write;

        let tmp = std::env::temp_dir().join("luxctl_test_ctx");
        std::fs::create_dir_all(&tmp).unwrap();
        let dockerfile = tmp.join("Dockerfile.test");
        let mut f = std::fs::File::create(&dockerfile).unwrap();
        writeln!(f, "FROM alpine:latest").unwrap();
        writeln!(f, "RUN echo hello").unwrap();

        let executor = match DockerExecutor::new() {
            Ok(e) => e,
            Err(_) => return,
        };

        let tar_gz = executor
            .build_context_tarball(&dockerfile, tmp.to_str().unwrap())
            .expect("tarball should build");

        let decoder = GzDecoder::new(&tar_gz[..]);
        let mut archive = tar::Archive::new(decoder);
        let mut found_dockerfile = false;
        for entry in archive.entries().unwrap() {
            let entry = entry.unwrap();
            let path = entry.path().unwrap();
            if path.to_str() == Some("Dockerfile") {
                found_dockerfile = true;
            }
        }

        assert!(found_dockerfile, "tar must contain a Dockerfile entry");

        std::fs::remove_dir_all(&tmp).ok();
    }
}
