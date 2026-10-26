#![cfg(feature = "prompt")]

use goose_agent::prompt::{render, ExtensionInfo, SystemPromptBuilder, SYSTEM_TEMPLATE};
use goose_provider_types::goose_mode::GooseMode;

#[test]
fn host_contributions_are_literal_ordered_and_sanitized() {
    let template = "{{ current_date_time }} / {{ goose_mode }} / {{ is_autonomous }}\n{% for e in extensions %}{{ e.name }}: {{ e.instructions }}\n{% endfor %}";
    let prompt = SystemPromptBuilder::new(template, "host supplied time", GooseMode::SmartApprove)
        .with_extensions([
            ExtensionInfo::new("z", "last", false),
            ExtensionInfo::new("a", "cafe\u{301}\u{e0041} {{ not_a_template }}", false),
        ])
        .with_prompt_extras([
            ("first".into(), "old".into()),
            ("second".into(), "literal {{ unclosed\u{e0042}".into()),
            ("first".into(), "replacement".into()),
            ("hints".into(), "stale hint".into()),
        ])
        .with_hints(Some("fresh hint".into()))
        .build()
        .unwrap();
    assert_eq!(prompt, "host supplied time / smart_approve / False\na: café {{ not_a_template }}\nz: last\n\n# Additional Instructions:\n\nreplacement\n\nliteral {{ unclosed\n\nfresh hint");
}

#[test]
fn chat_restriction_only_applies_to_chat_mode() {
    for mode in [
        GooseMode::Auto,
        GooseMode::Approve,
        GooseMode::SmartApprove,
        GooseMode::Chat,
    ] {
        let prompt = SystemPromptBuilder::new(SYSTEM_TEMPLATE, "", mode)
            .without_extensions()
            .build()
            .unwrap();
        assert_eq!(
            prompt.contains("no access to any tool"),
            mode == GooseMode::Chat
        );
        assert!(!prompt.contains("No extensions are defined"));
    }
}

#[test]
fn template_context_cannot_close_a_code_fence_or_become_template_source() {
    let prompt = render(
        "{{ body | code_fence }}",
        &serde_json::json!({
            "body": "```\n{{ unclosed\n````\n"
        }),
    )
    .unwrap();
    assert_eq!(prompt, "`````\n```\n{{ unclosed\n````\n`````");
}
