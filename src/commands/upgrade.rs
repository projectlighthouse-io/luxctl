use color_eyre::eyre::{bail, Result, WrapErr};
use flate2::read::GzDecoder;
use std::fs;
use std::io::Read;
use tar::Archive;

use crate::VERSION;

const GITHUB_REPO: &str = "thearyanahmed/luxctl";

#[derive(serde::Deserialize)]
struct GitHubRelease {
    tag_name: String,
}

/// normalize version string: strip leading 'v' or 'V'
fn normalize_version(v: &str) -> &str {
    v.strip_prefix('v')
        .or_else(|| v.strip_prefix('V'))
        .unwrap_or(v)
}

/// parse "major.minor.patch" into a tuple for comparison
fn parse_semver(v: &str) -> Result<(u64, u64, u64)> {
    let parts: Vec<&str> = v.split('.').collect();
    if parts.len() != 3 {
        bail!("invalid version format: {v} (expected X.Y.Z)");
    }

    let major = parts[0]
        .parse::<u64>()
        .wrap_err_with(|| format!("invalid major version: {}", parts[0]))?;
    let minor = parts[1]
        .parse::<u64>()
        .wrap_err_with(|| format!("invalid minor version: {}", parts[1]))?;
    let patch = parts[2]
        .parse::<u64>()
        .wrap_err_with(|| format!("invalid patch version: {}", parts[2]))?;

    Ok((major, minor, patch))
}

/// determine the asset filename for the current platform
fn asset_name() -> Result<String> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    match (os, arch) {
        ("linux", "x86_64") => Ok("luxctl-linux-x86_64.tar.gz".to_string()),
        ("linux", "aarch64") => Ok("luxctl-linux-aarch64.tar.gz".to_string()),
        ("macos", "aarch64") => Ok("luxctl-macos-aarch64.tar.gz".to_string()),
        ("macos", "x86_64") => Ok("luxctl-macos-x86_64.tar.gz".to_string()),
        // not `cargo install`: a source build has no client secret and the api
        // rejects it.
        _ => bail!(
            "can't upgrade in place on {os}/{arch}. download the latest release from:\n  https://github.com/{GITHUB_REPO}/releases"
        ),
    }
}

/// fetch the latest release tag from github
async fn fetch_latest_version() -> Result<String> {
    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");

    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("User-Agent", format!("luxctl/{VERSION}"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .wrap_err("failed to reach GitHub API")?;

    if !resp.status().is_success() {
        bail!(
            "GitHub API returned {}: {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        );
    }

    let release: GitHubRelease = resp.json().await.wrap_err("failed to parse release JSON")?;
    Ok(release.tag_name)
}

pub async fn run(version: Option<String>) -> Result<()> {
    println!("current version: {VERSION}");

    // resolve target version
    let target_tag = if let Some(v) = version {
        let normalized = normalize_version(&v);
        format!("v{normalized}")
    } else {
        println!("checking for latest release...");
        fetch_latest_version().await?
    };

    let target_version = normalize_version(&target_tag);

    // compare versions
    let current = parse_semver(VERSION)?;
    let target = parse_semver(target_version)?;

    if current == target {
        println!("already up to date ({VERSION})");
        return Ok(());
    }

    if current > target {
        println!(
            "warning: target version {target_version} is older than current {VERSION}. downgrading."
        );
    }

    println!("upgrading to {target_version}...");

    // determine asset + download URL
    let asset = asset_name()?;
    let download_url =
        format!("https://github.com/{GITHUB_REPO}/releases/download/{target_tag}/{asset}");

    // download the tarball
    let client = reqwest::Client::new();
    let resp = client
        .get(&download_url)
        .header("User-Agent", format!("luxctl/{VERSION}"))
        .send()
        .await
        .wrap_err("failed to download release")?;

    if !resp.status().is_success() {
        bail!(
            "download failed (HTTP {}). is version {target_version} published?",
            resp.status()
        );
    }

    let bytes = resp
        .bytes()
        .await
        .wrap_err("failed to read response body")?;

    // extract binary from tarball
    let gz = GzDecoder::new(&bytes[..]);
    let mut archive = Archive::new(gz);

    let mut binary_data: Option<Vec<u8>> = None;
    for entry in archive.entries().wrap_err("failed to read tarball")? {
        let mut entry = entry.wrap_err("corrupt tarball entry")?;
        let path = entry
            .path()
            .wrap_err("invalid path in tarball")?
            .to_path_buf();

        // the binary is the entry whose name starts with "luxctl"
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        if name.starts_with("luxctl") && !name.ends_with(".tar.gz") {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .wrap_err("failed to read binary from tarball")?;
            binary_data = Some(buf);
            break;
        }
    }

    let binary_data = match binary_data {
        Some(d) => d,
        None => bail!("could not find luxctl binary inside the tarball"),
    };

    // locate current binary and write new one atomically
    let current_exe =
        std::env::current_exe().wrap_err("could not determine current binary path")?;
    let current_exe =
        fs::canonicalize(&current_exe).wrap_err("could not resolve current binary path")?;

    let parent = current_exe
        .parent()
        .ok_or_else(|| color_eyre::eyre::eyre!("binary has no parent directory"))?;

    let temp_path = parent.join(".luxctl-upgrade-tmp");

    // write new binary to temp file
    fs::write(&temp_path, &binary_data).wrap_err_with(|| {
        format!(
            "failed to write to {}. try: sudo luxctl upgrade",
            temp_path.display()
        )
    })?;

    // set executable permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o755);
        fs::set_permissions(&temp_path, perms).wrap_err("failed to set executable permissions")?;
    }

    // atomic rename
    fs::rename(&temp_path, &current_exe).wrap_err_with(|| {
        // clean up temp file on failure
        let _ = fs::remove_file(&temp_path);
        format!(
            "failed to replace binary at {}. try: sudo luxctl upgrade",
            current_exe.display()
        )
    })?;

    println!("upgraded to {target_version}");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_version() {
        assert_eq!(normalize_version("v0.9.0"), "0.9.0");
        assert_eq!(normalize_version("V0.9.0"), "0.9.0");
        assert_eq!(normalize_version("0.9.0"), "0.9.0");
        assert_eq!(normalize_version("v1.2.3"), "1.2.3");
    }

    #[test]
    fn test_parse_semver_valid() {
        assert_eq!(parse_semver("0.9.0").ok(), Some((0, 9, 0)));
        assert_eq!(parse_semver("1.0.0").ok(), Some((1, 0, 0)));
        assert_eq!(parse_semver("12.34.56").ok(), Some((12, 34, 56)));
    }

    #[test]
    fn test_parse_semver_invalid() {
        assert!(parse_semver("0.9").is_err());
        assert!(parse_semver("abc").is_err());
        assert!(parse_semver("1.2.3.4").is_err());
        assert!(parse_semver("a.b.c").is_err());
    }

    #[test]
    fn test_version_comparison() {
        let v090 = parse_semver("0.9.0").ok();
        let v091 = parse_semver("0.9.1").ok();
        let v100 = parse_semver("1.0.0").ok();

        assert!(v090 < v091);
        assert!(v091 < v100);
        assert!(v091 == v091);
        assert!(v100 > v090);
    }

    #[test]
    fn test_asset_name_current_platform() {
        // should succeed on linux x86_64, linux aarch64, or macos aarch64
        // and fail on anything else (like macos x86_64 or windows)
        let result = asset_name();
        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;

        match (os, arch) {
            ("linux", "x86_64") => {
                assert_eq!(result.as_deref().ok(), Some("luxctl-linux-x86_64.tar.gz"));
            }
            ("linux", "aarch64") => {
                assert_eq!(result.as_deref().ok(), Some("luxctl-linux-aarch64.tar.gz"));
            }
            ("macos", "aarch64") => {
                assert_eq!(result.as_deref().ok(), Some("luxctl-macos-aarch64.tar.gz"));
            }
            _ => {
                assert!(result.is_err());
            }
        }
    }
}
