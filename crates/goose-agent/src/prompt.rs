//! System prompt composition shared with Goose. Hosts supply template text,
//! time, mode and contributions; this module never reads configuration or files.

use goose_provider_types::{goose_mode::GooseMode, utils::sanitize_unicode_tags};
use indexmap::IndexMap;
use minijinja::{Environment, Error, Value};
use serde::Serialize;

pub const SYSTEM_TEMPLATE: &str = include_str!("../prompts/system.md");
const CHAT_INSTRUCTIONS: &str = include_str!("../prompts/chat_mode.md");

#[derive(Clone, Debug, Serialize)]
pub struct ExtensionInfo {
    pub name: String,
    pub instructions: String,
    pub has_resources: bool,
}

impl ExtensionInfo {
    pub fn new(name: &str, instructions: &str, has_resources: bool) -> Self {
        Self {
            name: name.to_string(),
            instructions: instructions.to_string(),
            has_resources,
        }
    }
}

#[derive(Debug, Serialize)]
struct SystemPromptContext<'a> {
    extensions: Vec<ExtensionInfo>,
    current_date_time: &'a str,
    goose_mode: GooseMode,
    is_autonomous: bool,
    code_execution_mode: bool,
    include_extensions: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    moim_system_prompt_block: Option<String>,
}

/// Composes an explicitly supplied template with ordered operation contributions.
/// Replacing a contribution preserves its position, matching Goose's session extras.
#[derive(Debug)]
pub struct SystemPromptBuilder<'a> {
    template: &'a str,
    context: SystemPromptContext<'a>,
    extras: IndexMap<String, String>,
    hints: Option<String>,
}

impl<'a> SystemPromptBuilder<'a> {
    pub fn new(template: &'a str, current_date_time: &'a str, mode: GooseMode) -> Self {
        Self {
            template,
            context: SystemPromptContext {
                extensions: Vec::new(),
                current_date_time,
                goose_mode: mode,
                is_autonomous: mode == GooseMode::Auto,
                code_execution_mode: false,
                include_extensions: true,
                moim_system_prompt_block: None,
            },
            extras: IndexMap::new(),
            hints: None,
        }
    }

    #[must_use]
    pub fn with_extensions(mut self, extensions: impl IntoIterator<Item = ExtensionInfo>) -> Self {
        self.context.extensions.extend(extensions);
        self
    }

    #[must_use]
    pub fn with_prompt_extras(
        mut self,
        extras: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        self.extras.extend(extras);
        self
    }

    /// Supplies already-loaded repository instructions. File access remains with the host.
    #[must_use]
    pub fn with_hints(mut self, hints: Option<String>) -> Self {
        self.hints = hints;
        self
    }

    #[must_use]
    pub fn with_code_execution_mode(mut self, enabled: bool) -> Self {
        self.context.code_execution_mode = enabled;
        self
    }

    #[must_use]
    pub fn without_extensions(mut self) -> Self {
        self.context.include_extensions = false;
        self
    }

    /// Supplies the optional system block used by Goose's MOIM integration.
    #[must_use]
    pub fn with_moim_block(mut self, block: Option<String>) -> Self {
        self.context.moim_system_prompt_block = block;
        self
    }

    /// Render the base template, then append sanitized contributions as literal text.
    ///
    /// # Errors
    /// Returns an error when the supplied template cannot be parsed or rendered.
    pub fn build(mut self) -> anyhow::Result<String> {
        // Stable ordering lets independent sessions reuse the same prompt prefix.
        self.context.extensions.sort_by(|a, b| a.name.cmp(&b.name));
        for extension in &mut self.context.extensions {
            extension.instructions = sanitize_unicode_tags(&extension.instructions);
        }
        let base = render(&sanitize_unicode_tags(self.template), &self.context)
            .map_err(|error| anyhow::anyhow!("Could not render system prompt: {error}"))?;
        if let Some(hints) = self.hints {
            self.extras.insert("hints".to_string(), hints);
        }
        if self.context.goose_mode == GooseMode::Chat {
            self.extras.insert(
                "chat_mode".to_string(),
                CHAT_INSTRUCTIONS.trim().to_string(),
            );
        }
        if self.extras.is_empty() {
            Ok(base)
        } else {
            let extras = self
                .extras
                .into_values()
                .map(|extra| sanitize_unicode_tags(&extra))
                .collect::<Vec<_>>()
                .join("\n\n");
            Ok(format!("{base}\n\n# Additional Instructions:\n\n{extras}"))
        }
    }
}

/// Render a host-supplied template using Goose's whitespace and code-fence rules.
/// Context values are data, not recursively evaluated template source.
///
/// # Errors
/// Returns the template parsing or rendering error.
pub fn render<T: Serialize>(template: &str, context: &T) -> Result<String, Error> {
    let mut environment = Environment::new();
    environment.set_trim_blocks(true);
    environment.set_lstrip_blocks(true);
    environment.add_filter("code_fence", code_fence);
    environment.add_template("template", template)?;
    let rendered = environment
        .get_template("template")?
        .render(Value::from_serialize(context))?;
    Ok(rendered.trim().to_string())
}

fn code_fence(code: String) -> String {
    let longest_run = code
        .chars()
        .fold((0usize, 0usize), |(max, run), c| {
            if c == '`' {
                (max.max(run + 1), run + 1)
            } else {
                (max, 0)
            }
        })
        .0;
    let fence = "`".repeat((longest_run + 1).max(3));
    format!("{fence}\n{}\n{fence}", code.trim_end_matches('\n'))
}
