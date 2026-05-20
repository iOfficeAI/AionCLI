//! Pre-install ACP agent packages at startup so runtime spawns
//! only perform read operations — avoiding Windows EBUSY conflicts
//! when multiple bun processes try to copy from shared cache.

use std::path::{Path, PathBuf};

use tracing::{error, info, warn};

use crate::spawn::Builder;

const INSTALL_STAMP: &str = ".install-stamp";
const ACP_PACKAGES_DIR: &str = "acp-packages";

/// Metadata for a pre-installable ACP package.
#[derive(Debug, Clone)]
pub struct AcpPackageSpec {
    pub package: String,
    pub version: String,
}

impl AcpPackageSpec {
    /// Short directory name: last segment of scoped package + version.
    /// e.g. `@agentclientprotocol/claude-agent-acp` → `claude-agent-acp@0.33.1`
    fn dir_name(&self) -> String {
        let short = self.package.rsplit('/').next().unwrap_or(&self.package);
        format!("{short}@{}", self.version)
    }
}

/// Returns the install directory path for a given package spec.
pub fn package_dir(data_dir: &Path, spec: &AcpPackageSpec) -> PathBuf {
    data_dir.join(ACP_PACKAGES_DIR).join(spec.dir_name())
}

/// Check if a package version is already installed (stamp file exists).
pub fn is_installed(data_dir: &Path, spec: &AcpPackageSpec) -> bool {
    package_dir(data_dir, spec).join(INSTALL_STAMP).is_file()
}

/// Resolve the entry-point script path for a pre-installed package.
/// Returns `None` if not installed.
pub fn entry_point(data_dir: &Path, spec: &AcpPackageSpec) -> Option<PathBuf> {
    if !is_installed(data_dir, spec) {
        return None;
    }
    let dir = package_dir(data_dir, spec);
    // Standard entry: node_modules/{package}/dist/index.js
    let entry = dir
        .join("node_modules")
        .join(&spec.package)
        .join("dist")
        .join("index.js");
    if entry.is_file() {
        return Some(entry);
    }
    // Fallback: look for bin entry in node_modules/.bin
    let bin_entry = dir
        .join("node_modules")
        .join(".bin")
        .join(spec.package.rsplit('/').next().unwrap_or(&spec.package));
    if bin_entry.is_file() {
        return Some(bin_entry);
    }
    None
}

/// Install a package into its versioned directory.
/// Uses atomic rename pattern: installs to `.tmp` dir, renames on success.
pub async fn install(data_dir: &Path, spec: &AcpPackageSpec) -> Result<(), InstallError> {
    let target = package_dir(data_dir, spec);
    let tmp_dir = target.with_extension("tmp");

    // Clean up any leftover partial install
    if tmp_dir.exists() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    std::fs::create_dir_all(&tmp_dir).map_err(|e| InstallError::Io(format!("create tmp dir: {e}")))?;

    // Write package.json
    let pkg_json = format!(
        r#"{{"private":true,"dependencies":{{"{package}":"{version}"}}}}"#,
        package = spec.package,
        version = spec.version,
    );
    std::fs::write(tmp_dir.join("package.json"), pkg_json)
        .map_err(|e| InstallError::Io(format!("write package.json: {e}")))?;

    // Run bun install
    let mut cmd = Builder::clean_cli("bun");
    cmd.arg("install").current_dir(&tmp_dir);

    let output = cmd
        .output()
        .await
        .map_err(|e| InstallError::Spawn(format!("failed to spawn bun install: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: String = stderr
            .chars()
            .rev()
            .take(500)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(InstallError::BunInstall(format!(
            "bun install exited with {}: {}",
            output.status, tail
        )));
    }

    // Verify entry point exists before committing
    let entry = tmp_dir
        .join("node_modules")
        .join(&spec.package)
        .join("dist")
        .join("index.js");
    if !entry.is_file() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(InstallError::MissingEntry(format!(
            "expected entry point not found: {}",
            entry.display()
        )));
    }

    // Write install stamp
    std::fs::write(tmp_dir.join(INSTALL_STAMP), "").map_err(|e| InstallError::Io(format!("write stamp: {e}")))?;

    // Atomic rename: remove old target (if any), rename tmp → target
    if target.exists() {
        let _ = std::fs::remove_dir_all(&target);
    }
    std::fs::rename(&tmp_dir, &target).map_err(|e| {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        InstallError::Io(format!("rename tmp to target: {e}"))
    })?;

    Ok(())
}

/// Remove stale versions. Keeps only packages whose version matches
/// one of the provided specs.
pub fn cleanup_stale(data_dir: &Path, active_specs: &[&AcpPackageSpec]) {
    let packages_dir = data_dir.join(ACP_PACKAGES_DIR);
    let Ok(entries) = std::fs::read_dir(&packages_dir) else {
        return;
    };

    let active_names: Vec<String> = active_specs.iter().map(|s| s.dir_name()).collect();

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // Skip .tmp directories (in-progress installs)
        if name.ends_with(".tmp") {
            continue;
        }
        if !active_names.contains(&name) {
            let path = entry.path();
            info!(dir = %path.display(), "removing stale version");
            if let Err(e) = std::fs::remove_dir_all(&path) {
                warn!(
                    dir = %path.display(),
                    error = %e,
                    "failed to remove stale version, skipping"
                );
            }
        }
    }
}

/// Parse a `bun x` args JSON into an `AcpPackageSpec`.
///
/// Expected formats:
/// - `["x","--bun","@scope/pkg@version"]`
/// - `["x","--bun","@scope/pkg@version","--extra-flags"]`
pub fn parse_bun_x_args(args_json: &str) -> Option<AcpPackageSpec> {
    let args: Vec<String> = serde_json::from_str(args_json).ok()?;

    // Find the package specifier: first arg after "--bun" that starts with '@' or a letter
    let bun_idx = args.iter().position(|a| a == "--bun")?;
    let pkg_arg = args.get(bun_idx + 1)?;

    // Split "package@version" — handle scoped packages like @scope/name@version
    let (package, version) = split_package_version(pkg_arg)?;

    Some(AcpPackageSpec { package, version })
}

/// Split `@scope/name@version` or `name@version` into (package, version).
fn split_package_version(s: &str) -> Option<(String, String)> {
    // For scoped packages: @scope/name@version
    // The version separator is the last '@' that isn't at position 0
    let at_pos = if let Some(rest) = s.strip_prefix('@') {
        // Scoped: find '@' after the scope prefix
        rest.rfind('@').map(|p| p + 1)
    } else {
        s.rfind('@')
    }?;

    let package = s[..at_pos].to_string();
    let version = s[at_pos + 1..].to_string();

    if package.is_empty() || version.is_empty() {
        return None;
    }

    Some((package, version))
}

/// Ensure all ACP packages from the provided specs are installed.
/// Logs progress and errors; never panics.
pub async fn ensure_packages(data_dir: &Path, specs: &[AcpPackageSpec]) {
    for spec in specs {
        if is_installed(data_dir, spec) {
            info!(
                package = %spec.package,
                version = %spec.version,
                "package up to date, skipping install"
            );
            continue;
        }

        info!(
            package = %spec.package,
            version = %spec.version,
            dir = %package_dir(data_dir, spec).display(),
            "installing package"
        );

        let start = std::time::Instant::now();
        match install(data_dir, spec).await {
            Ok(()) => {
                info!(
                    package = %spec.package,
                    version = %spec.version,
                    elapsed_ms = start.elapsed().as_millis() as u64,
                    "install completed"
                );
            }
            Err(e) => {
                error!(
                    package = %spec.package,
                    version = %spec.version,
                    error = %e,
                    "install failed, will fallback to bun-x at runtime"
                );
            }
        }
    }

    let active_refs: Vec<&AcpPackageSpec> = specs.iter().collect();
    cleanup_stale(data_dir, &active_refs);
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("io: {0}")]
    Io(String),
    #[error("spawn: {0}")]
    Spawn(String),
    #[error("bun install failed: {0}")]
    BunInstall(String),
    #[error("missing entry point: {0}")]
    MissingEntry(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_name_scoped_package() {
        let spec = AcpPackageSpec {
            package: "@agentclientprotocol/claude-agent-acp".into(),
            version: "0.33.1".into(),
        };
        assert_eq!(spec.dir_name(), "claude-agent-acp@0.33.1");
    }

    #[test]
    fn dir_name_unscoped_package() {
        let spec = AcpPackageSpec {
            package: "some-package".into(),
            version: "1.2.3".into(),
        };
        assert_eq!(spec.dir_name(), "some-package@1.2.3");
    }

    #[test]
    fn package_dir_format() {
        let spec = AcpPackageSpec {
            package: "@agentclientprotocol/claude-agent-acp".into(),
            version: "0.33.1".into(),
        };
        let dir = package_dir(Path::new("/data"), &spec);
        assert_eq!(dir, PathBuf::from("/data/acp-packages/claude-agent-acp@0.33.1"));
    }

    #[test]
    fn is_installed_false_when_no_stamp() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec = AcpPackageSpec {
            package: "@agentclientprotocol/claude-agent-acp".into(),
            version: "0.33.1".into(),
        };
        assert!(!is_installed(tmp.path(), &spec));
    }

    #[test]
    fn is_installed_true_when_stamp_exists() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec = AcpPackageSpec {
            package: "@agentclientprotocol/claude-agent-acp".into(),
            version: "0.33.1".into(),
        };
        let dir = package_dir(tmp.path(), &spec);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(INSTALL_STAMP), "").unwrap();
        assert!(is_installed(tmp.path(), &spec));
    }

    #[test]
    fn entry_point_returns_none_when_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec = AcpPackageSpec {
            package: "@agentclientprotocol/claude-agent-acp".into(),
            version: "0.33.1".into(),
        };
        assert_eq!(entry_point(tmp.path(), &spec), None);
    }

    #[test]
    fn entry_point_returns_path_when_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let spec = AcpPackageSpec {
            package: "@agentclientprotocol/claude-agent-acp".into(),
            version: "0.33.1".into(),
        };
        let dir = package_dir(tmp.path(), &spec);
        let entry_dir = dir
            .join("node_modules")
            .join("@agentclientprotocol/claude-agent-acp")
            .join("dist");
        std::fs::create_dir_all(&entry_dir).unwrap();
        std::fs::write(entry_dir.join("index.js"), "// entry").unwrap();
        std::fs::write(dir.join(INSTALL_STAMP), "").unwrap();

        let result = entry_point(tmp.path(), &spec);
        assert_eq!(result, Some(entry_dir.join("index.js")));
    }

    #[test]
    fn parse_bun_x_args_claude() {
        let json = r#"["x","--bun","@agentclientprotocol/claude-agent-acp@0.33.1"]"#;
        let spec = parse_bun_x_args(json).unwrap();
        assert_eq!(spec.package, "@agentclientprotocol/claude-agent-acp");
        assert_eq!(spec.version, "0.33.1");
    }

    #[test]
    fn parse_bun_x_args_codex() {
        let json = r#"["x","--bun","@zed-industries/codex-acp@0.14.0"]"#;
        let spec = parse_bun_x_args(json).unwrap();
        assert_eq!(spec.package, "@zed-industries/codex-acp");
        assert_eq!(spec.version, "0.14.0");
    }

    #[test]
    fn parse_bun_x_args_with_extra_flags() {
        let json = r#"["x","--bun","@tencent-ai/codebuddy-code@2.97.0","--acp"]"#;
        let spec = parse_bun_x_args(json).unwrap();
        assert_eq!(spec.package, "@tencent-ai/codebuddy-code");
        assert_eq!(spec.version, "2.97.0");
    }

    #[test]
    fn parse_bun_x_args_invalid_returns_none() {
        assert!(parse_bun_x_args(r#"["run","index.js"]"#).is_none());
        assert!(parse_bun_x_args(r#"invalid json"#).is_none());
        assert!(parse_bun_x_args(r#"["x","--bun"]"#).is_none());
    }

    #[test]
    fn split_package_version_scoped() {
        let (pkg, ver) = split_package_version("@scope/name@1.2.3").unwrap();
        assert_eq!(pkg, "@scope/name");
        assert_eq!(ver, "1.2.3");
    }

    #[test]
    fn split_package_version_unscoped() {
        let (pkg, ver) = split_package_version("name@1.2.3").unwrap();
        assert_eq!(pkg, "name");
        assert_eq!(ver, "1.2.3");
    }

    #[test]
    fn split_package_version_no_version() {
        assert!(split_package_version("@scope/name").is_none());
        assert!(split_package_version("name").is_none());
    }

    #[test]
    fn cleanup_stale_removes_inactive() {
        let tmp = tempfile::TempDir::new().unwrap();
        let packages_dir = tmp.path().join(ACP_PACKAGES_DIR);
        std::fs::create_dir_all(&packages_dir).unwrap();

        // Create active and stale directories
        std::fs::create_dir_all(packages_dir.join("claude-agent-acp@0.33.1")).unwrap();
        std::fs::create_dir_all(packages_dir.join("claude-agent-acp@0.32.0")).unwrap();
        std::fs::create_dir_all(packages_dir.join("codex-acp@0.14.0")).unwrap();

        let active = AcpPackageSpec {
            package: "@agentclientprotocol/claude-agent-acp".into(),
            version: "0.33.1".into(),
        };
        let active2 = AcpPackageSpec {
            package: "@zed-industries/codex-acp".into(),
            version: "0.14.0".into(),
        };

        cleanup_stale(tmp.path(), &[&active, &active2]);

        assert!(packages_dir.join("claude-agent-acp@0.33.1").exists());
        assert!(packages_dir.join("codex-acp@0.14.0").exists());
        assert!(!packages_dir.join("claude-agent-acp@0.32.0").exists());
    }
}
