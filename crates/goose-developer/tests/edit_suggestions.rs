use goose_developer::edit::string_replace;

#[test]
fn rejected_edit_shows_the_matching_block_after_unrelated_blank_lines() {
    let block = "function renderTask(task) {\n  const label = \"Task: \" +\n    task.name;\n  return label;\n}";
    let content = format!(
        "import {{ format }} from './format.js';\n\nfunction noop()\n{{\n  return null;\n}}\n{}\n{block}\n",
        "// unrelated code\n".repeat(30)
    );
    let reformatted = block.replace("\"Task: \" +\n    task.name", "\"Task: \"\n    + task.name");

    for before in [reformatted.clone(), format!("\n{reformatted}")] {
        let error = string_replace(&content, &before, "replacement").unwrap_err();
        let suggestion = error
            .split("Did you mean:\n```\n")
            .nth(1)
            .and_then(|text| text.split("\n```").next())
            .unwrap_or_default();
        assert!(suggestion.contains(block), "{error}");
        assert!(!suggestion.contains("import"), "{error}");
    }
}
