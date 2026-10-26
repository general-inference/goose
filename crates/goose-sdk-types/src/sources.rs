//! Transport-independent source metadata used by discovery and rendering.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The type of source entity.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum SourceType {
    #[default]
    Skill,
    BuiltinSkill,
    Recipe,
    Subrecipe,
    Agent,
    Project,
}

impl std::fmt::Display for SourceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceType::Skill => write!(f, "skill"),
            SourceType::BuiltinSkill => write!(f, "builtin skill"),
            SourceType::Recipe => write!(f, "recipe"),
            SourceType::Subrecipe => write!(f, "subrecipe"),
            SourceType::Agent => write!(f, "agent"),
            SourceType::Project => write!(f, "project"),
        }
    }
}

/// A source discovered by Goose. Filesystem sources use an on-disk path;
/// built-in sources use a stable synthetic path. Sources may be either
/// `global` (shared across all projects) or project-specific.
#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SourceEntry {
    #[serde(rename = "type")]
    pub source_type: SourceType,
    pub name: String,
    pub description: String,
    pub content: String,
    /// Stable on-disk path identifying this source. Pass it back to
    /// update/delete/export to operate on this entry. Skills use the directory
    /// containing `SKILL.md`; projects use the project file path; built-in
    /// skills use `builtin://skills/<name>` synthetic paths.
    pub path: String,
    /// True when the source lives in the user's global sources directory; false
    /// when it lives inside a specific project.
    pub global: bool,
    /// True when this source can be modified through source CRUD methods.
    /// Client-provided bundled sources are returned as read-only.
    #[serde(default)]
    pub writable: bool,
    /// Paths (absolute) of additional files that live alongside the source.
    /// Only skills currently populate this; empty for other source types.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supporting_files: Vec<String>,
    /// Arbitrary key/value pairs for type-specific metadata (e.g. icon, color,
    /// preferredProvider for projects). Stored in the frontmatter.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub properties: std::collections::HashMap<String, serde_json::Value>,
}

impl SourceEntry {
    /// Render this source as a markdown block suitable for injecting into an
    /// LLM context. Used by the skills and summon runtimes when loading a
    /// source into the current conversation.
    pub fn to_load_text(&self) -> String {
        format!(
            "## {} ({})\n\n{}\n\n### Content\n\n{}",
            self.name, self.source_type, self.description, self.content
        )
    }
}
