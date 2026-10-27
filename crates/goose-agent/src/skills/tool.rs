//! Goose's load_skill definition and dispatch, with host-controlled supporting-file reads.

use super::{loaded_skill_context, skill_content_with_args, SourceEntry, SourceType};
use anyhow::Result;
use rmcp::model::{CallToolResult, ContentBlock, JsonObject, Tool};
use serde::Deserialize;
use serde_json::Value;
use std::io;
use std::path::{Path, PathBuf};

pub const DEFAULT_MAX_CHARACTERS: usize = 200_000;

/// Hosts enforce filesystem access during the read and may postprocess skill content.
pub trait SkillLoader {
    fn render_skill(&self, skill: &SourceEntry, args: Option<&str>) -> Result<String> {
        Ok(loaded_skill_context(
            skill,
            &skill_content_with_args(skill, args)?,
        ))
    }

    /// Read UTF-8 text within the skill directory, rejecting oversized content.
    /// Confinement must hold during actual I/O, including symlink resolution.
    fn read_supporting_file(
        &self,
        skill_dir: &Path,
        relative: &Path,
        max_characters: usize,
    ) -> io::Result<String>;
}

pub fn skill_tool() -> Tool {
    Tool::new("load_skill", include_str!("load-skill.md"), serde_json::json!({
        "type": "object", "required": ["name"], "properties": {
            "name": {"type":"string", "description":"Name of the skill to load. Use \"skill-name/path\" to load a supporting file."},
            "args": {"type":["string", "null"], "description":"Optional arguments to provide when loading the skill."}
        }
    }).as_object().expect("object schema").clone())
}

#[derive(Deserialize)]
struct LoadSkillParams {
    /// Name of the skill to load. Use "skill-name/path" to load a supporting file.
    name: String,
    /// Optional arguments to provide when loading the skill.
    #[serde(default)]
    args: Option<String>,
}

pub fn load_skill(
    skills: &[SourceEntry],
    arguments: Option<JsonObject>,
    loader: &dyn SkillLoader,
    max_characters: usize,
) -> CallToolResult {
    let params = arguments
        .map(Value::Object)
        .ok_or_else(|| "Missing arguments".to_string())
        .and_then(|arguments| {
            serde_json::from_value::<LoadSkillParams>(arguments)
                .map_err(|error| format!("Invalid arguments: {error}"))
        });
    let params = match params {
        Ok(params) => params,
        Err(error) => return CallToolResult::error(vec![ContentBlock::text(error)]),
    };
    let skill_name = params.name.as_str();
    let args = params.args.as_deref();

    if let Some(skill) = skills.iter().find(|skill| skill.name == skill_name) {
        return match loader.render_skill(skill, args) {
            Ok(rendered) => CallToolResult::success(vec![ContentBlock::text(rendered)]),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(format!(
                "Failed to parse skill arguments: {error}"
            ))]),
        };
    }

    if let Some((parent_skill_name, raw_relative_path)) = skill_name.split_once('/') {
        let relative_path = raw_relative_path.replace('\\', "/");
        if let Some(skill) = skills.iter().find(|skill| {
            skill.name == parent_skill_name
                && matches!(
                    skill.source_type,
                    SourceType::Skill | SourceType::BuiltinSkill
                )
        }) {
            return select_supporting_file(
                skill,
                skill_name,
                &relative_path,
                loader,
                max_characters,
            );
        }
    }

    let suggestions: Vec<&str> = skills
        .iter()
        .filter(|skill| {
            skill
                .name
                .to_lowercase()
                .contains(&skill_name.to_lowercase())
                || skill_name
                    .to_lowercase()
                    .contains(&skill.name.to_lowercase())
        })
        .take(3)
        .map(|skill| skill.name.as_str())
        .collect();
    if suggestions.is_empty() {
        CallToolResult::error(vec![ContentBlock::text(format!(
            "Skill '{skill_name}' not found."
        ))])
    } else {
        CallToolResult::error(vec![ContentBlock::text(format!(
            "Skill '{skill_name}' not found. Did you mean: {}?",
            suggestions.join(", ")
        ))])
    }
}

fn select_supporting_file(
    skill: &SourceEntry,
    skill_name: &str,
    relative_path: &str,
    loader: &dyn SkillLoader,
    max_characters: usize,
) -> CallToolResult {
    let skill_dir = PathBuf::from(&skill.path);
    for file_path in &skill.supporting_files {
        let file_path = Path::new(file_path);
        let Ok(relative) = file_path.strip_prefix(&skill_dir) else {
            continue;
        };
        if relative.to_string_lossy().replace('\\', "/") != relative_path {
            continue;
        }
        return match load_supporting_file(&skill_dir, relative, skill_name, loader, max_characters)
        {
            Ok(content) => CallToolResult::success(vec![ContentBlock::text(content)]),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(format!(
                "Failed to read '{skill_name}': {error}"
            ))]),
        };
    }

    let available: Vec<String> = skill
        .supporting_files
        .iter()
        .filter_map(|file| {
            Path::new(file)
                .strip_prefix(&skill_dir)
                .ok()
                .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        })
        .take(10)
        .collect();
    if available.is_empty() {
        CallToolResult::error(vec![ContentBlock::text(format!(
            "Skill '{}' has no supporting files.",
            skill.name
        ))])
    } else {
        CallToolResult::error(vec![ContentBlock::text(format!(
            "File '{skill_name}' not found. Available: {}",
            available.join(", ")
        ))])
    }
}

pub fn load_supporting_file(
    dir: &Path,
    relative: &Path,
    name: &str,
    loader: &dyn SkillLoader,
    max_characters: usize,
) -> io::Result<String> {
    let prefix = format!("# Loaded: {name}\n\n");
    let suffix = "\n\n---\nFile loaded into context.";
    let content_limit = max_characters.checked_sub(prefix.chars().count() + suffix.chars().count())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("maximum tool response size of {max_characters} characters is too small to load '{name}'")))?;
    let content = loader.read_supporting_file(dir, relative, content_limit)?;
    if content.chars().count() > content_limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("supporting file exceeds the maximum size of {content_limit} characters"),
        ));
    }
    Ok(format!("{prefix}{content}{suffix}"))
}
