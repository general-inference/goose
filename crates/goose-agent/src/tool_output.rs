//! Shared large-response handling. Hosts own storage and access to saved output.
use rmcp::model::{CallToolResult, ContentBlock, ErrorData};

pub const DEFAULT_LARGE_TEXT_THRESHOLD: usize = 200_000;

/// Preserve result metadata and non-text blocks. Storage failures fail closed:
/// never send the oversized content back to inference as an error fallback.
/// The callback must return a reference the agent can read with its tools.
pub fn process_tool_response(
    response: Result<CallToolResult, ErrorData>,
    threshold: usize,
    mut save: impl FnMut(&str) -> std::io::Result<String>,
) -> Result<CallToolResult, ErrorData> {
    let mut result = response?;
    for content in &mut result.content {
        let Some(text) = content.as_text() else {
            continue;
        };
        let characters = text.text.chars().count();
        if characters <= threshold {
            continue;
        }
        let reference = save(&text.text)
            .map_err(|_| ErrorData::internal_error("Could not save oversized tool output", None))?;
        let notice = include_str!("prompts/tool_output.md")
            .replace("{{characters}}", &characters.to_string())
            .replace("{{reference}}", &reference);
        if let ContentBlock::Text(text) = content {
            text.text = notice;
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spills_unicode_without_losing_content_or_result_metadata() {
        let text = "🦆".repeat(11);
        let mut response = CallToolResult::error(vec![
            ContentBlock::text(text.clone()),
            ContentBlock::image("bytes", "image/png"),
        ]);
        response.structured_content = Some(serde_json::json!({"items":[1]}));
        let expected = response.clone();
        let result = process_tool_response(Ok(response), 10, |saved| {
            assert_eq!(saved, text);
            Ok("output.txt".into())
        })
        .unwrap();
        assert_eq!(result.is_error, expected.is_error);
        assert_eq!(result.structured_content, expected.structured_content);
        assert_eq!(result.content[1], expected.content[1]);
        assert!(result.content[0]
            .as_text()
            .unwrap()
            .text
            .ends_with("output.txt"));
        let small = CallToolResult::success(vec![ContentBlock::text("🦆".repeat(10))]);
        assert_eq!(
            process_tool_response(Ok(small.clone()), 10, |_| panic!("must not spill")).unwrap(),
            small
        );
    }
    #[test]
    fn storage_failure_never_leaks_oversized_output() {
        let result = process_tool_response(
            Ok(CallToolResult::success(vec![ContentBlock::text(
                "secret".repeat(100),
            )])),
            10,
            |_| Err(std::io::ErrorKind::PermissionDenied.into()),
        );
        assert!(!result.unwrap_err().message.contains("secret"));
    }
}
