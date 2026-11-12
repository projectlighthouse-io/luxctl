use crate::ui::UI;
use color_eyre::eyre::{Result, WrapErr};
use std::io::Read;
use std::path::Path;

const PROJECTFILES_ARCHIVE_URL: &str =
    "https://github.com/projectlighthouse-io/projectfiles/archive/refs/heads";

/// downloads project fixture files from the public projectfiles repo
/// and extracts them into the workspace directory.
///
/// each project has its own branch in the repo. the archive is fetched
/// via github's tarball endpoint (no git clone, no auth needed).
///
/// fixture files (logs/, config/, data/) are copied directly into the
/// workspace so the learner can reference them with relative paths
/// like `./logs/auth.log`.
pub async fn download_fixtures(project_slug: &str, workspace: &Path) -> Result<bool> {
    let url = format!("{}/{}.tar.gz", PROJECTFILES_ARCHIVE_URL, project_slug);

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .wrap_err("failed to create http client")?;

    let response = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            UI::warn(
                &format!("could not download project files for '{}'", project_slug),
                Some(&format!("{}", e)),
            );
            UI::note("retry with `luxctl sync` once you are back online");
            return Ok(false);
        }
    };

    // a 404 means the project has no fixture branch: nothing to download, so
    // nothing to warn about. most projects ship no files.
    // ponytail: a misnamed branch is silent too, and shows up as a task
    // failing on a missing ./logs/ file. check the blueprint for fixture
    // paths before deciding, if that ever bites.
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        log::debug!("no fixture branch for '{}'", project_slug);
        return Ok(false);
    }

    if !response.status().is_success() {
        UI::warn(
            &format!("no project files downloaded for '{}'", project_slug),
            Some(&format!("HTTP {}", response.status().as_u16())),
        );
        UI::note(
            "if this project's tasks reference files like ./logs/ or ./data/, report this — the files are missing, not your solution",
        );
        return Ok(false);
    }

    let bytes = response
        .bytes()
        .await
        .wrap_err("failed to read fixture archive")?;

    extract_archive(&bytes, project_slug, workspace)?;

    Ok(true)
}

fn extract_archive(bytes: &[u8], project_slug: &str, workspace: &Path) -> Result<()> {
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);

    // github archives have a top-level dir: projectfiles-{branch}/
    let prefix = format!("projectfiles-{}/", project_slug);

    for entry in archive
        .entries()
        .wrap_err("failed to read archive entries")?
    {
        let mut entry = entry.wrap_err("failed to read archive entry")?;
        let path = entry.path().wrap_err("failed to get entry path")?;
        let path_str = path.to_string_lossy().to_string();

        let relative = match path_str.strip_prefix(&prefix) {
            Some(r) if !r.is_empty() => r.to_string(),
            _ => continue,
        };

        let target = workspace.join(&relative);

        if entry.header().entry_type().is_dir() {
            std::fs::create_dir_all(&target)
                .wrap_err_with(|| format!("failed to create dir: {}", target.display()))?;
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut content = Vec::new();
            entry.read_to_end(&mut content)?;
            std::fs::write(&target, &content)
                .wrap_err_with(|| format!("failed to write: {}", target.display()))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;

    fn make_test_archive(slug: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut builder = tar::Builder::new(&mut encoder);
            let prefix = format!("projectfiles-{}", slug);

            // add top-level dir
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Directory);
            header.set_path(format!("{}/", prefix)).ok();
            header.set_size(0);
            header.set_cksum();
            builder.append(&header, &[] as &[u8]).ok();

            for (path, content) in files {
                let mut header = tar::Header::new_gnu();
                header.set_size(content.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(
                        &mut header,
                        format!("{}/{}", prefix, path),
                        *content as &[u8],
                    )
                    .ok();
            }
            builder.finish().ok();
        }
        encoder.finish().unwrap_or_default()
    }

    #[test]
    fn test_extract_archive_creates_files() {
        let tmp = tempfile::TempDir::new().unwrap_or_else(|e| panic!("{e}"));
        let archive = make_test_archive(
            "test-project",
            &[
                ("logs/auth.log", b"ERROR something failed\n"),
                ("config/app.conf", b"port = 8080\n"),
            ],
        );

        extract_archive(&archive, "test-project", tmp.path()).unwrap_or_else(|e| panic!("{e}"));

        let log_path = tmp.path().join("logs/auth.log");
        assert!(log_path.exists());
        let content = std::fs::read_to_string(&log_path).unwrap_or_default();
        assert_eq!(content, "ERROR something failed\n");

        let conf_path = tmp.path().join("config/app.conf");
        assert!(conf_path.exists());
    }

    #[test]
    fn test_extract_archive_ignores_wrong_prefix() {
        let tmp = tempfile::TempDir::new().unwrap_or_else(|e| panic!("{e}"));
        let archive = make_test_archive("other-project", &[("logs/auth.log", b"data\n")]);

        // extracting with wrong slug should create nothing
        extract_archive(&archive, "wrong-slug", tmp.path()).unwrap_or_else(|e| panic!("{e}"));

        assert!(!tmp.path().join("logs/auth.log").exists());
    }
}
