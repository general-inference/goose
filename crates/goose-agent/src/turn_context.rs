//! Goose's provider-visible operational context, composed from host-supplied values.
use goose_provider_types::conversation::message::{Message, MessageMetadata};
use goose_provider_types::conversation::{
    CURRENT_TIME_TAG, TURN_CONTEXT_TAG, WORKING_DIRECTORY_TAG,
};
use std::path::Path;

const MIN_CONTEXT_FOR_MOIM: usize = 32_000;

pub fn system_prompt_block() -> String {
    include_str!("../prompts/turn_context.md").replace("{turn_context_tag}", TURN_CONTEXT_TAG)
}

/// Hosts supply a stable timestamp for the turn and persist the agent-only event.
/// This does not read the clock, workspace, configuration or extension state.
pub fn turn_context_event(
    working_dir: &Path,
    context_limit: Option<usize>,
    parts: Vec<String>,
    timestamp: &str,
) -> Option<Message> {
    if should_skip(context_limit) {
        return None;
    }
    Some(
        Message::user()
            .with_text(compose(working_dir, parts, timestamp))
            .with_metadata(MessageMetadata::agent_only().with_turn_context()),
    )
}

/// Suppress unchanged context within a turn, preserving Goose's event timing.
pub fn update(event: Option<Message>, messages: &[Message]) -> Option<Message> {
    let last = messages
        .iter()
        .rev()
        .find(|message| message.is_turn_context())
        .map(Message::as_concat_text);
    event.filter(|message| Some(message.as_concat_text()) != last)
}

pub fn should_skip(context_limit: Option<usize>) -> bool {
    context_limit.is_some_and(|limit| limit < MIN_CONTEXT_FOR_MOIM)
}

fn compose(working_dir: &Path, parts: Vec<String>, timestamp: &str) -> String {
    let mut lines = vec![
        open_tag(TURN_CONTEXT_TAG),
        tag(CURRENT_TIME_TAG, timestamp),
        tag(WORKING_DIRECTORY_TAG, &working_dir.display().to_string()),
    ];

    for part in parts {
        if !part.trim().is_empty() {
            lines.push(String::new());
            lines.push(part);
        }
    }

    lines.push(close_tag(TURN_CONTEXT_TAG));
    lines.join("\n")
}

fn open_tag(name: &str) -> String {
    format!("<{name}>")
}

fn close_tag(name: &str) -> String {
    format!("</{name}>")
}

fn tag(name: &str, value: &str) -> String {
    format!("<{name}>{}</{name}>", escape_xml_text(value))
}

fn escape_xml_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn turn_budget_part(turns_taken: u32, max_turns: u32) -> Option<String> {
    if max_turns == 0 || turns_taken.saturating_mul(2) < max_turns {
        return None;
    }

    Some(tag(
        "turn-budget",
        &format!("{turns_taken}/{max_turns} used"),
    ))
}
