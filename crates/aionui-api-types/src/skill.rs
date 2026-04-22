use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// A. Skill list & info
// ---------------------------------------------------------------------------

/// Origin of a listed skill — `builtin`, `custom`, or `extension`.
///
/// Matches the renderer contract in
/// `src/common/adapter/ipcBridge.ts::listAvailableSkills`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SkillSourceResponse {
    Builtin,
    Custom,
    Extension,
}

/// Single item in the available skills list (`GET /api/skills`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillListItemResponse {
    pub name: String,
    pub description: String,
    pub location: String,
    pub is_custom: bool,
    pub source: SkillSourceResponse,
}

/// Request body for `POST /api/skills/info`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadSkillInfoRequest {
    pub skill_path: String,
}

/// Response for `POST /api/skills/info`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReadSkillInfoResponse {
    pub name: String,
    pub description: String,
}

// ---------------------------------------------------------------------------
// B. Skill import / export / delete
// ---------------------------------------------------------------------------

/// Request body for `POST /api/skills/import` and `POST /api/skills/import-symlink`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSkillRequest {
    pub skill_path: String,
}

/// Response for skill import operations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImportSkillResponse {
    pub skill_name: String,
}

/// Request body for `POST /api/skills/export-symlink`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportSkillRequest {
    pub skill_path: String,
    pub target_dir: String,
}

/// Request body for `DELETE /api/skills/:name` (path param, but also usable as body).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteSkillRequest {
    pub skill_name: String,
}

// ---------------------------------------------------------------------------
// C. Skill scanning & discovery
// ---------------------------------------------------------------------------

/// Request body for `POST /api/skills/scan`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanForSkillsRequest {
    pub folder_path: String,
}

/// A skill discovered by directory scanning.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ScannedSkillResponse {
    pub name: String,
    pub description: String,
    pub path: String,
}

/// Response for `POST /api/skills/scan`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ScanForSkillsResponse {
    pub skills: Vec<ScannedSkillResponse>,
}

/// An external skill source with count (`GET /api/skills/detect-external`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExternalSkillSourceResponse {
    pub name: String,
    pub path: String,
    pub skill_count: usize,
    pub skills: Vec<ScannedSkillResponse>,
}

/// A named filesystem path (`GET /api/skills/detect-paths`, external paths).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NamedPathResponse {
    pub name: String,
    pub path: String,
}

/// Response for `GET /api/skills/paths`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillPathsResponse {
    pub user_skills_dir: String,
    pub builtin_skills_dir: String,
}

// ---------------------------------------------------------------------------
// D. Assistant rules & skills
// ---------------------------------------------------------------------------

/// Request body for `POST /api/skills/assistant-rule/read` and
/// `POST /api/skills/assistant-skill/read`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadAssistantRuleRequest {
    pub assistant_id: String,
    #[serde(default)]
    pub locale: Option<String>,
}

/// Request body for `POST /api/skills/assistant-rule/write` and
/// `POST /api/skills/assistant-skill/write`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteAssistantRuleRequest {
    pub assistant_id: String,
    pub content: String,
    #[serde(default)]
    pub locale: Option<String>,
}

/// Request body for `POST /api/skills/builtin-rule` and
/// `POST /api/skills/builtin-skill`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadBuiltinResourceRequest {
    pub file_name: String,
}

// ---------------------------------------------------------------------------
// E. External path management
// ---------------------------------------------------------------------------

/// Request body for `POST /api/skills/external-paths`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddExternalPathRequest {
    pub name: String,
    pub path: String,
}

/// Request body for `DELETE /api/skills/external-paths`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveExternalPathRequest {
    pub path: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -- Skill list --

    #[test]
    fn test_skill_list_item_serde() {
        let item = SkillListItemResponse {
            name: "my-skill".into(),
            description: "Does things".into(),
            location: "/home/user/.aionui/skills/my-skill".into(),
            is_custom: true,
            source: SkillSourceResponse::Custom,
        };
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(json["name"], "my-skill");
        assert_eq!(json["isCustom"], true);
        assert_eq!(json["source"], "custom");
        assert!(json.get("is_custom").is_none());
    }

    #[test]
    fn test_skill_source_serializes_lowercase() {
        assert_eq!(
            serde_json::to_value(SkillSourceResponse::Builtin).unwrap(),
            serde_json::json!("builtin")
        );
        assert_eq!(
            serde_json::to_value(SkillSourceResponse::Custom).unwrap(),
            serde_json::json!("custom")
        );
        assert_eq!(
            serde_json::to_value(SkillSourceResponse::Extension).unwrap(),
            serde_json::json!("extension")
        );
    }

    #[test]
    fn test_read_skill_info_request() {
        let raw = json!({"skillPath": "/path/to/skill"});
        let req: ReadSkillInfoRequest = serde_json::from_value(raw).unwrap();
        assert_eq!(req.skill_path, "/path/to/skill");
    }

    #[test]
    fn test_read_skill_info_response() {
        let resp = ReadSkillInfoResponse {
            name: "test".into(),
            description: "A test skill".into(),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["name"], "test");
        assert_eq!(json["description"], "A test skill");
    }

    // -- Import / Export --

    #[test]
    fn test_import_skill_request() {
        let raw = json!({"skillPath": "/external/skill"});
        let req: ImportSkillRequest = serde_json::from_value(raw).unwrap();
        assert_eq!(req.skill_path, "/external/skill");
    }

    #[test]
    fn test_import_skill_response() {
        let resp = ImportSkillResponse {
            skill_name: "imported-skill".into(),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["skillName"], "imported-skill");
    }

    #[test]
    fn test_export_skill_request() {
        let raw = json!({"skillPath": "/user/skill", "targetDir": "/external/dir"});
        let req: ExportSkillRequest = serde_json::from_value(raw).unwrap();
        assert_eq!(req.skill_path, "/user/skill");
        assert_eq!(req.target_dir, "/external/dir");
    }

    // -- Scanning --

    #[test]
    fn test_scan_for_skills_request() {
        let raw = json!({"folderPath": "/some/dir"});
        let req: ScanForSkillsRequest = serde_json::from_value(raw).unwrap();
        assert_eq!(req.folder_path, "/some/dir");
    }

    #[test]
    fn test_scanned_skill_response() {
        let skill = ScannedSkillResponse {
            name: "found-skill".into(),
            description: "Found during scan".into(),
            path: "/dir/found-skill".into(),
        };
        let json = serde_json::to_value(&skill).unwrap();
        assert_eq!(json["name"], "found-skill");
        assert_eq!(json["path"], "/dir/found-skill");
    }

    #[test]
    fn test_external_skill_source_response() {
        let source = ExternalSkillSourceResponse {
            name: "Claude Skills".into(),
            path: "/home/user/.claude/skills".into(),
            skill_count: 2,
            skills: vec![
                ScannedSkillResponse {
                    name: "s1".into(),
                    description: "d1".into(),
                    path: "/p1".into(),
                },
                ScannedSkillResponse {
                    name: "s2".into(),
                    description: "d2".into(),
                    path: "/p2".into(),
                },
            ],
        };
        let json = serde_json::to_value(&source).unwrap();
        assert_eq!(json["skillCount"], 2);
        assert_eq!(json["skills"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_named_path_response() {
        let path = NamedPathResponse {
            name: "Claude Config".into(),
            path: "/home/user/.claude".into(),
        };
        let json = serde_json::to_value(&path).unwrap();
        assert_eq!(json["name"], "Claude Config");
        assert_eq!(json["path"], "/home/user/.claude");
    }

    #[test]
    fn test_skill_paths_response() {
        let resp = SkillPathsResponse {
            user_skills_dir: "/home/user/.aionui/skills".into(),
            builtin_skills_dir: "/app/resources/skills".into(),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["userSkillsDir"], "/home/user/.aionui/skills");
        assert_eq!(json["builtinSkillsDir"], "/app/resources/skills");
    }

    // -- Assistant rules --

    #[test]
    fn test_read_assistant_rule_request_with_locale() {
        let raw = json!({"assistantId": "abc123", "locale": "zh-CN"});
        let req: ReadAssistantRuleRequest = serde_json::from_value(raw).unwrap();
        assert_eq!(req.assistant_id, "abc123");
        assert_eq!(req.locale.as_deref(), Some("zh-CN"));
    }

    #[test]
    fn test_read_assistant_rule_request_without_locale() {
        let raw = json!({"assistantId": "abc123"});
        let req: ReadAssistantRuleRequest = serde_json::from_value(raw).unwrap();
        assert!(req.locale.is_none());
    }

    #[test]
    fn test_write_assistant_rule_request() {
        let raw = json!({
            "assistantId": "abc123",
            "content": "# Rules\nBe helpful.",
            "locale": "en-US"
        });
        let req: WriteAssistantRuleRequest = serde_json::from_value(raw).unwrap();
        assert_eq!(req.assistant_id, "abc123");
        assert_eq!(req.content, "# Rules\nBe helpful.");
        assert_eq!(req.locale.as_deref(), Some("en-US"));
    }

    #[test]
    fn test_read_builtin_resource_request() {
        let raw = json!({"fileName": "code-review.md"});
        let req: ReadBuiltinResourceRequest = serde_json::from_value(raw).unwrap();
        assert_eq!(req.file_name, "code-review.md");
    }

    // -- External paths --

    #[test]
    fn test_add_external_path_request() {
        let raw = json!({"name": "My Skills", "path": "/path/to/skills"});
        let req: AddExternalPathRequest = serde_json::from_value(raw).unwrap();
        assert_eq!(req.name, "My Skills");
        assert_eq!(req.path, "/path/to/skills");
    }

    #[test]
    fn test_remove_external_path_request() {
        let raw = json!({"path": "/path/to/skills"});
        let req: RemoveExternalPathRequest = serde_json::from_value(raw).unwrap();
        assert_eq!(req.path, "/path/to/skills");
    }
}
