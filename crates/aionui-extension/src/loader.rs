use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use crate::constants::{EXTENSION_API_VERSION, EXTENSION_MANIFEST_FILE, EXTENSIONS_DIR_NAME};
use crate::manifest::parse_manifest;
use crate::types::{ExtensionSource, ExtensionState, LoadedExtension};

// ---------------------------------------------------------------------------
// Scan path resolution
// ---------------------------------------------------------------------------

/// A scan path paired with its source classification.
#[derive(Debug, Clone)]
pub struct ScanPath {
    pub path: PathBuf,
    pub source: ExtensionSource,
}

/// Resolve the default list of directories to scan for extensions.
///
/// Priority (highest first):
/// 1. `$AIONUI_EXTENSIONS_PATH` — colon-separated list of directories
/// 2. `~/.aionui/extensions/` — user data directory
/// 3. Platform AppData directory
///
/// In E2E test mode (`AIONUI_E2E_TEST=1`), only the environment variable
/// paths are returned to ensure test isolation.
pub fn resolve_scan_paths() -> Vec<ScanPath> {
    let mut paths = Vec::new();

    // 1. Environment variable paths (highest priority).
    if let Ok(env_paths) = std::env::var("AIONUI_EXTENSIONS_PATH") {
        for p in env_paths.split(':') {
            let trimmed = p.trim();
            if !trimmed.is_empty() {
                paths.push(ScanPath {
                    path: PathBuf::from(trimmed),
                    source: ExtensionSource::Env,
                });
            }
        }
    }

    // E2E test mode: only scan env var paths for isolation.
    if is_e2e_test_mode() {
        return paths;
    }

    // 2. User data directory (~/.aionui/extensions/).
    if let Some(home) = dirs::home_dir() {
        let user_dir = home.join(".aionui").join(EXTENSIONS_DIR_NAME);
        paths.push(ScanPath {
            path: user_dir,
            source: ExtensionSource::Local,
        });
    }

    // 3. AppData directory (platform-specific).
    if let Some(data_dir) = dirs::data_dir() {
        let appdata_dir = data_dir.join("aionui").join(EXTENSIONS_DIR_NAME);
        paths.push(ScanPath {
            path: appdata_dir,
            source: ExtensionSource::Appdata,
        });
    }

    paths
}

// ---------------------------------------------------------------------------
// Extension loading
// ---------------------------------------------------------------------------

/// Scan all provided directories and load valid extension manifests.
///
/// When the same extension name appears in multiple scan paths, the first
/// occurrence wins (earlier entries have higher priority).
pub fn load_all(scan_paths: &[ScanPath]) -> Vec<LoadedExtension> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut result: Vec<LoadedExtension> = Vec::new();

    for sp in scan_paths {
        let loaded = scan_directory(&sp.path, sp.source);
        for ext in loaded {
            let name = ext.manifest.name.clone();
            if let std::collections::hash_map::Entry::Vacant(e) = seen.entry(name.clone()) {
                e.insert(result.len());
                result.push(ext);
            } else {
                debug!(
                    name = %name,
                    skipped_path = %sp.path.display(),
                    "skipping duplicate extension (higher-priority copy already loaded)"
                );
            }
        }
    }

    result
}

/// Scan a single directory for extension subdirectories containing a
/// valid manifest file.
fn scan_directory(dir: &Path, source: ExtensionSource) -> Vec<LoadedExtension> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                warn!(dir = %dir.display(), error = %e, "failed to read extensions directory");
            }
            return Vec::new();
        }
    };

    let mut loaded = Vec::new();

    for entry in entries.flatten() {
        let entry_path = entry.path();
        if !entry_path.is_dir() {
            continue;
        }

        let manifest_path = entry_path.join(EXTENSION_MANIFEST_FILE);
        match load_single_extension(&manifest_path, &entry_path, source) {
            Ok(ext) => {
                debug!(name = %ext.manifest.name, dir = %entry_path.display(), "loaded extension");
                loaded.push(ext);
            }
            Err(e) => {
                // Skip extensions with invalid manifests but continue loading others.
                warn!(
                    dir = %entry_path.display(),
                    error = %e,
                    "skipping extension with invalid manifest"
                );
            }
        }
    }

    loaded
}

/// Load a single extension from its manifest file.
fn load_single_extension(
    manifest_path: &Path,
    ext_dir: &Path,
    source: ExtensionSource,
) -> Result<LoadedExtension, crate::error::ExtensionError> {
    let bytes = std::fs::read(manifest_path)?;
    let manifest = parse_manifest(&bytes)?;

    let state = ExtensionState {
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        enabled: true,
        installed_at: None,
        last_activated_at: None,
    };

    let directory = ext_dir
        .to_str()
        .unwrap_or_default()
        .to_owned();

    Ok(LoadedExtension {
        manifest,
        directory,
        source,
        state,
    })
}

// ---------------------------------------------------------------------------
// Engine compatibility filtering
// ---------------------------------------------------------------------------

/// Filter extensions by engine and API version compatibility.
///
/// Extensions that declare `engine.aionui` with a version range incompatible
/// with `app_version` are excluded. Extensions whose `apiVersion` is
/// incompatible with the supported [`EXTENSION_API_VERSION`] are also excluded.
///
/// Incompatible extensions are logged as warnings but do not cause errors.
pub fn filter_by_engine_compatibility(
    extensions: Vec<LoadedExtension>,
    app_version: &str,
) -> Vec<LoadedExtension> {
    let Ok(app_ver) = semver::Version::parse(app_version) else {
        warn!(
            app_version = %app_version,
            "invalid app version — skipping engine compatibility filter"
        );
        return extensions;
    };

    extensions
        .into_iter()
        .filter(|ext| {
            is_engine_compatible(ext, &app_ver) && is_api_version_compatible(ext)
        })
        .collect()
}

/// Check whether the extension's `engine.aionui` requirement is satisfied.
fn is_engine_compatible(ext: &LoadedExtension, app_version: &semver::Version) -> bool {
    let Some(engine) = &ext.manifest.engine else {
        return true; // no engine constraint
    };
    let Some(required) = &engine.aionui else {
        return true; // no aionui constraint
    };

    match semver::VersionReq::parse(required) {
        Ok(req) if req.matches(app_version) => true,
        Ok(_) => {
            warn!(
                name = %ext.manifest.name,
                required = %required,
                actual = %app_version,
                "extension filtered out: engine.aionui incompatible"
            );
            false
        }
        Err(e) => {
            warn!(
                name = %ext.manifest.name,
                required = %required,
                error = %e,
                "extension filtered out: invalid engine.aionui version requirement"
            );
            false
        }
    }
}

/// Check whether the extension's `apiVersion` is compatible with the
/// supported API version.
fn is_api_version_compatible(ext: &LoadedExtension) -> bool {
    let Some(api_ver_str) = &ext.manifest.api_version else {
        return true; // no API version constraint
    };

    let Ok(declared) = semver::Version::parse(api_ver_str) else {
        warn!(
            name = %ext.manifest.name,
            api_version = %api_ver_str,
            "extension filtered out: invalid apiVersion"
        );
        return false;
    };

    let Ok(supported) = semver::Version::parse(EXTENSION_API_VERSION) else {
        return true; // defensive — should never happen with a valid constant
    };

    // Compatible if major versions match and declared <= supported.
    if declared.major == supported.major && declared <= supported {
        true
    } else {
        warn!(
            name = %ext.manifest.name,
            declared = %declared,
            supported = %supported,
            "extension filtered out: apiVersion incompatible"
        );
        false
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn is_e2e_test_mode() -> bool {
    std::env::var("AIONUI_E2E_TEST")
        .map(|v| v == "1")
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EngineConfig, ExtensionManifest};
    use std::fs;
    use tempfile::TempDir;

    /// Helper: create a minimal valid manifest JSON.
    fn write_manifest(dir: &Path, name: &str, version: &str) {
        write_manifest_full(dir, name, version, None, None);
    }

    /// Helper: create a manifest with optional engine and apiVersion fields.
    fn write_manifest_full(
        dir: &Path,
        name: &str,
        version: &str,
        engine_aionui: Option<&str>,
        api_version: Option<&str>,
    ) {
        let mut manifest = serde_json::json!({
            "name": name,
            "version": version,
        });
        if let Some(eng) = engine_aionui {
            manifest["engine"] = serde_json::json!({ "aionui": eng });
        }
        if let Some(api) = api_version {
            manifest["apiVersion"] = serde_json::json!(api);
        }
        let manifest_path = dir.join(EXTENSION_MANIFEST_FILE);
        fs::write(manifest_path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    }

    // -- scan_directory -------------------------------------------------------

    #[test]
    fn scan_empty_directory() {
        let tmp = TempDir::new().unwrap();
        let result = scan_directory(tmp.path(), ExtensionSource::Local);
        assert!(result.is_empty());
    }

    #[test]
    fn scan_nonexistent_directory() {
        let result = scan_directory(Path::new("/nonexistent/path"), ExtensionSource::Local);
        assert!(result.is_empty());
    }

    #[test]
    fn scan_loads_valid_extension() {
        let tmp = TempDir::new().unwrap();
        let ext_dir = tmp.path().join("my-ext");
        fs::create_dir(&ext_dir).unwrap();
        write_manifest(&ext_dir, "my-ext", "1.0.0");

        let result = scan_directory(tmp.path(), ExtensionSource::Local);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].manifest.name, "my-ext");
        assert_eq!(result[0].manifest.version, "1.0.0");
        assert_eq!(result[0].source, ExtensionSource::Local);
        assert!(result[0].state.enabled);
    }

    #[test]
    fn scan_skips_invalid_manifest() {
        let tmp = TempDir::new().unwrap();

        // Valid extension
        let good_dir = tmp.path().join("good-ext");
        fs::create_dir(&good_dir).unwrap();
        write_manifest(&good_dir, "good-ext", "1.0.0");

        // Invalid extension (bad JSON)
        let bad_dir = tmp.path().join("bad-ext");
        fs::create_dir(&bad_dir).unwrap();
        fs::write(
            bad_dir.join(EXTENSION_MANIFEST_FILE),
            b"not valid json",
        )
        .unwrap();

        let result = scan_directory(tmp.path(), ExtensionSource::Env);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].manifest.name, "good-ext");
    }

    #[test]
    fn scan_skips_directories_without_manifest() {
        let tmp = TempDir::new().unwrap();
        let ext_dir = tmp.path().join("no-manifest");
        fs::create_dir(&ext_dir).unwrap();
        fs::write(ext_dir.join("README.md"), b"hello").unwrap();

        let result = scan_directory(tmp.path(), ExtensionSource::Local);
        assert!(result.is_empty());
    }

    #[test]
    fn scan_skips_files_not_directories() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("not-a-dir.txt"), b"hello").unwrap();

        let result = scan_directory(tmp.path(), ExtensionSource::Local);
        assert!(result.is_empty());
    }

    // -- load_all -------------------------------------------------------------

    #[test]
    fn load_all_deduplicates_by_name() {
        let tmp1 = TempDir::new().unwrap();
        let tmp2 = TempDir::new().unwrap();

        // Same extension name in two directories
        let ext1 = tmp1.path().join("my-ext");
        fs::create_dir(&ext1).unwrap();
        write_manifest(&ext1, "my-ext", "1.0.0");

        let ext2 = tmp2.path().join("my-ext");
        fs::create_dir(&ext2).unwrap();
        write_manifest(&ext2, "my-ext", "2.0.0");

        let scan_paths = vec![
            ScanPath {
                path: tmp1.path().to_path_buf(),
                source: ExtensionSource::Env,
            },
            ScanPath {
                path: tmp2.path().to_path_buf(),
                source: ExtensionSource::Local,
            },
        ];

        let result = load_all(&scan_paths);
        assert_eq!(result.len(), 1);
        // First occurrence wins (higher priority).
        assert_eq!(result[0].manifest.version, "1.0.0");
        assert_eq!(result[0].source, ExtensionSource::Env);
    }

    #[test]
    fn load_all_from_multiple_directories() {
        let tmp1 = TempDir::new().unwrap();
        let tmp2 = TempDir::new().unwrap();

        let ext1 = tmp1.path().join("ext-a");
        fs::create_dir(&ext1).unwrap();
        write_manifest(&ext1, "ext-a", "1.0.0");

        let ext2 = tmp2.path().join("ext-b");
        fs::create_dir(&ext2).unwrap();
        write_manifest(&ext2, "ext-b", "1.0.0");

        let scan_paths = vec![
            ScanPath {
                path: tmp1.path().to_path_buf(),
                source: ExtensionSource::Env,
            },
            ScanPath {
                path: tmp2.path().to_path_buf(),
                source: ExtensionSource::Local,
            },
        ];

        let result = load_all(&scan_paths);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn load_all_empty_paths() {
        let result = load_all(&[]);
        assert!(result.is_empty());
    }

    // -- filter_by_engine_compatibility ----------------------------------------

    fn make_loaded_ext(
        name: &str,
        version: &str,
        engine_aionui: Option<&str>,
        api_version: Option<&str>,
    ) -> LoadedExtension {
        LoadedExtension {
            manifest: ExtensionManifest {
                name: name.to_string(),
                version: version.to_string(),
                display_name: None,
                description: None,
                author: None,
                license: None,
                homepage: None,
                icon: None,
                engine: engine_aionui.map(|v| EngineConfig {
                    aionui: Some(v.to_string()),
                }),
                api_version: api_version.map(|v| v.to_string()),
                dependencies: HashMap::new(),
                entry_point: None,
                permissions: None,
                contributes: None,
                lifecycle: None,
                i18n: None,
            },
            directory: format!("/test/{name}"),
            source: ExtensionSource::Local,
            state: ExtensionState {
                name: name.to_string(),
                version: version.to_string(),
                enabled: true,
                installed_at: None,
                last_activated_at: None,
            },
        }
    }

    #[test]
    fn filter_keeps_compatible_engine() {
        let exts = vec![make_loaded_ext("ext-a", "1.0.0", Some("^1.0.0"), None)];
        let filtered = filter_by_engine_compatibility(exts, "1.5.0");
        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn filter_removes_incompatible_engine() {
        let exts = vec![make_loaded_ext("ext-a", "1.0.0", Some("^2.0.0"), None)];
        let filtered = filter_by_engine_compatibility(exts, "1.5.0");
        assert!(filtered.is_empty());
    }

    #[test]
    fn filter_keeps_no_engine_constraint() {
        let exts = vec![make_loaded_ext("ext-a", "1.0.0", None, None)];
        let filtered = filter_by_engine_compatibility(exts, "1.5.0");
        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn filter_keeps_compatible_api_version() {
        let exts = vec![make_loaded_ext("ext-a", "1.0.0", None, Some("1.0.0"))];
        let filtered = filter_by_engine_compatibility(exts, "1.0.0");
        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn filter_removes_incompatible_api_version() {
        // Extension requires API 2.0.0 but we support 1.0.0
        let exts = vec![make_loaded_ext("ext-a", "1.0.0", None, Some("2.0.0"))];
        let filtered = filter_by_engine_compatibility(exts, "1.0.0");
        assert!(filtered.is_empty());
    }

    #[test]
    fn filter_removes_invalid_engine_requirement() {
        let exts = vec![make_loaded_ext(
            "ext-a",
            "1.0.0",
            Some("not-valid-semver-req"),
            None,
        )];
        let filtered = filter_by_engine_compatibility(exts, "1.0.0");
        assert!(filtered.is_empty());
    }

    #[test]
    fn filter_keeps_all_with_invalid_app_version() {
        // If the app version itself is invalid, skip filtering entirely.
        let exts = vec![make_loaded_ext("ext-a", "1.0.0", Some("^2.0.0"), None)];
        let filtered = filter_by_engine_compatibility(exts, "not-semver");
        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn filter_mixed_compatible_and_incompatible() {
        let exts = vec![
            make_loaded_ext("compatible", "1.0.0", Some("^1.0.0"), Some("1.0.0")),
            make_loaded_ext("bad-engine", "1.0.0", Some("^3.0.0"), None),
            make_loaded_ext("bad-api", "1.0.0", None, Some("2.0.0")),
            make_loaded_ext("no-constraint", "1.0.0", None, None),
        ];
        let filtered = filter_by_engine_compatibility(exts, "1.5.0");
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].manifest.name, "compatible");
        assert_eq!(filtered[1].manifest.name, "no-constraint");
    }

    // -- resolve_scan_paths ----------------------------------------------------

    #[test]
    fn resolve_scan_paths_includes_env_paths() {
        // Temporarily set the env var for this test.
        let _guard = EnvGuard::new("AIONUI_EXTENSIONS_PATH", "/tmp/test-exts");
        let _e2e_guard = EnvGuard::remove("AIONUI_E2E_TEST");
        let paths = resolve_scan_paths();
        assert!(paths.iter().any(|sp| sp.path.as_path() == Path::new("/tmp/test-exts")
            && sp.source == ExtensionSource::Env));
    }

    #[test]
    fn resolve_scan_paths_e2e_mode_only_env() {
        let _guard = EnvGuard::new("AIONUI_E2E_TEST", "1");
        let _ext_guard = EnvGuard::new("AIONUI_EXTENSIONS_PATH", "/tmp/e2e-exts");
        let paths = resolve_scan_paths();
        assert!(paths.iter().all(|sp| sp.source == ExtensionSource::Env));
    }

    // -- test helpers ---------------------------------------------------------

    /// RAII guard to set/restore an environment variable for a single test.
    struct EnvGuard {
        key: String,
        original: Option<String>,
    }

    impl EnvGuard {
        fn new(key: &str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            // SAFETY: test-only code running in serial (env var tests should
            // not be run concurrently with other env-dependent tests).
            unsafe { std::env::set_var(key, value) };
            Self {
                key: key.to_owned(),
                original,
            }
        }

        fn remove(key: &str) -> Self {
            let original = std::env::var(key).ok();
            // SAFETY: test-only code running in serial.
            unsafe { std::env::remove_var(key) };
            Self {
                key: key.to_owned(),
                original,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.original {
                // SAFETY: test-only code running in serial.
                Some(val) => unsafe { std::env::set_var(&self.key, val) },
                None => unsafe { std::env::remove_var(&self.key) },
            }
        }
    }
}
