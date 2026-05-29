use std::path::PathBuf;

use aionui_api_types::AgentMetadata;
use aionui_common::{AppError, ErrorChain};
use tokio::fs;
use tracing::info;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CodexSandboxMode {
    WorkspaceWrite,
    DangerFullAccess,
}

impl CodexSandboxMode {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }
}

pub(super) fn sandbox_mode_for_requested_mode(mode: Option<&str>) -> CodexSandboxMode {
    match mode.map(str::trim) {
        Some("full-access" | "yoloNoSandbox") => CodexSandboxMode::DangerFullAccess,
        _ => CodexSandboxMode::WorkspaceWrite,
    }
}

pub(super) async fn sync_for_agent(metadata: &AgentMetadata, requested_mode: Option<&str>) -> Result<(), AppError> {
    if metadata.backend.as_deref() != Some("codex") {
        return Ok(());
    }

    let sandbox_mode = sandbox_mode_for_requested_mode(requested_mode);
    write_codex_sandbox_mode(sandbox_mode).await?;
    info!(
        requested_mode = requested_mode.unwrap_or_default(),
        sandbox_mode = sandbox_mode.as_str(),
        "Codex sandbox config synced"
    );
    Ok(())
}

pub(super) async fn write_codex_sandbox_mode(mode: CodexSandboxMode) -> Result<(), AppError> {
    let path = codex_config_path()?;
    write_codex_sandbox_mode_to_path(mode, &path).await
}

async fn write_codex_sandbox_mode_to_path(mode: CodexSandboxMode, path: &std::path::Path) -> Result<(), AppError> {
    let content = fs::read_to_string(&path).await.unwrap_or_default();
    let rendered = render_config_with_sandbox_mode(&content, mode.as_str());

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| AppError::Internal(format!("Failed to create Codex config directory: {}", ErrorChain(&e))))?;
    }

    fs::write(&path, rendered)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to write Codex sandbox config: {}", ErrorChain(&e))))?;
    Ok(())
}

fn codex_config_path() -> Result<PathBuf, AppError> {
    if let Some(home) = std::env::var_os("CODEX_HOME")
        && !home.is_empty()
    {
        return Ok(PathBuf::from(home).join("config.toml"));
    }

    let home = dirs::home_dir()
        .ok_or_else(|| AppError::Internal("Failed to resolve home directory for Codex config".into()))?;
    Ok(home.join(".codex").join("config.toml"))
}

fn render_config_with_sandbox_mode(content: &str, mode: &str) -> String {
    let newline = if content.contains("\r\n") { "\r\n" } else { "\n" };
    let sandbox_line = format!("sandbox_mode = \"{mode}\"");
    let mut replaced = false;
    let mut lines = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim_start();
        let is_sandbox_line = trimmed
            .strip_prefix("sandbox_mode")
            .is_some_and(|rest| rest.trim_start().starts_with('='));
        if is_sandbox_line {
            lines.push(sandbox_line.clone());
            replaced = true;
        } else {
            lines.push(line.to_owned());
        }
    }

    let rendered = if replaced {
        let mut rendered = lines.join(newline);
        if content.ends_with('\n') {
            rendered.push_str(newline);
        }
        rendered
    } else if content.trim_start().starts_with('[') {
        format!("{sandbox_line}{newline}{newline}{content}")
    } else if let Some(section_index) = content.find("\n[") {
        let split_at = section_index + 1;
        let prefix = content[..split_at].trim_end();
        let suffix = &content[split_at..];
        if prefix.is_empty() {
            format!("{sandbox_line}{newline}{newline}{suffix}")
        } else {
            format!("{prefix}{newline}{sandbox_line}{newline}{newline}{suffix}")
        }
    } else if content.trim().is_empty() {
        format!("{sandbox_line}{newline}")
    } else {
        format!("{}{newline}{sandbox_line}{newline}", content.trim_end())
    };

    if mode == CodexSandboxMode::DangerFullAccess.as_str() {
        ensure_windows_unelevated_sandbox(&rendered, newline)
    } else {
        rendered
    }
}

fn ensure_windows_unelevated_sandbox(content: &str, newline: &str) -> String {
    let sandbox_line = "sandbox = \"unelevated\"";
    let mut lines: Vec<String> = content.lines().map(ToOwned::to_owned).collect();
    let Some(windows_start) = lines.iter().position(|line| line.trim() == "[windows]") else {
        let mut rendered = content.trim_end().to_owned();
        if !rendered.is_empty() {
            rendered.push_str(newline);
            rendered.push_str(newline);
        }
        rendered.push_str("[windows]");
        rendered.push_str(newline);
        rendered.push_str(sandbox_line);
        rendered.push_str(newline);
        return rendered;
    };

    let windows_end = lines
        .iter()
        .enumerate()
        .skip(windows_start + 1)
        .find_map(|(index, line)| line.trim_start().starts_with('[').then_some(index))
        .unwrap_or(lines.len());

    if let Some(sandbox_index) = lines[windows_start + 1..windows_end]
        .iter()
        .position(|line| {
            line.trim_start()
                .strip_prefix("sandbox")
                .is_some_and(|rest| rest.trim_start().starts_with('='))
        })
        .map(|offset| windows_start + 1 + offset)
    {
        lines[sandbox_index] = sandbox_line.to_owned();
    } else {
        lines.insert(windows_start + 1, sandbox_line.to_owned());
    }

    let mut rendered = lines.join(newline);
    if content.ends_with('\n') {
        rendered.push_str(newline);
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_access_maps_to_danger_full_access() {
        assert_eq!(
            sandbox_mode_for_requested_mode(Some("full-access")).as_str(),
            "danger-full-access"
        );
    }

    #[test]
    fn non_full_access_modes_map_to_workspace_write() {
        for mode in [None, Some(""), Some("auto"), Some("read-only"), Some("default")] {
            assert_eq!(sandbox_mode_for_requested_mode(mode).as_str(), "workspace-write");
        }
    }

    #[test]
    fn config_render_replaces_existing_sandbox_mode() {
        let input = r#"model = "gpt-5"
sandbox_mode = "read-only"

[tools]
web_search = true
"#;

        let rendered = render_config_with_sandbox_mode(input, "danger-full-access");

        assert!(rendered.contains(r#"sandbox_mode = "danger-full-access""#));
        assert!(!rendered.contains(r#"sandbox_mode = "read-only""#));
        assert!(rendered.contains("[tools]"));
    }

    #[test]
    fn config_render_inserts_before_first_section() {
        let input = r#"[tools]
web_search = true
"#;

        let rendered = render_config_with_sandbox_mode(input, "workspace-write");

        assert!(rendered.starts_with("sandbox_mode = \"workspace-write\"\n\n[tools]"));
    }

    #[test]
    fn config_render_full_access_adds_windows_unelevated_sandbox() {
        let input = r#"model = "gpt-5"

[tools]
web_search = true
"#;

        let rendered = render_config_with_sandbox_mode(input, "danger-full-access");

        assert!(rendered.contains("[windows]\nsandbox = \"unelevated\"\n"));
    }

    #[test]
    fn config_render_workspace_write_does_not_touch_windows_section() {
        let input = r#"sandbox_mode = "danger-full-access"

[windows]
sandbox = "unelevated"
other = true

[tools]
web_search = true
"#;

        let rendered = render_config_with_sandbox_mode(input, "workspace-write");

        assert!(rendered.contains("[windows]\nsandbox = \"unelevated\"\nother = true"));
    }

    #[test]
    fn config_render_full_access_updates_existing_windows_sandbox() {
        let input = r#"sandbox_mode = "workspace-write"

[windows]
sandbox = "elevated"
other = true

[tools]
web_search = true
"#;

        let rendered = render_config_with_sandbox_mode(input, "danger-full-access");

        assert!(rendered.contains("[windows]\nsandbox = \"unelevated\"\nother = true"));
        assert!(!rendered.contains("sandbox = \"elevated\""));
    }

    #[tokio::test]
    async fn write_codex_sandbox_mode_to_path_creates_parent_and_writes_full_access() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("nested").join("config.toml");

        write_codex_sandbox_mode_to_path(CodexSandboxMode::DangerFullAccess, &config_path)
            .await
            .unwrap();

        let rendered = fs::read_to_string(config_path).await.unwrap();
        assert_eq!(
            rendered,
            "sandbox_mode = \"danger-full-access\"\n\n[windows]\nsandbox = \"unelevated\"\n"
        );
    }
}
