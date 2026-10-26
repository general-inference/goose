use super::super::filesystem::NativeHintFileSystem;

use ignore::gitignore::GitignoreBuilder;

use super::*;

#[test]
fn test_parse_file_references() {
    let content = r#"
        Basic file references: @README.md @./docs/guide.md @../shared/config.json @/absolute/path/file.txt
        Inline references: @file1.txt and @file2.py
        Files with extensions: @component.tsx @file.test.js @config.local.json
        Files without extensions: @Makefile @LICENSE @Dockerfile @CHANGELOG
        Complex paths: @src/utils/helper.js @docs/api/endpoints.md
        
        Should not match:
        - Email addresses: user@example.com admin@company.org
        - Social handles: @username @user123
        - URLs: https://example.com/@user
        "#;

    let references = parse_file_references(content);

    // Should match expected file references
    let expected_files = [
        "README.md",
        "./docs/guide.md",
        "../shared/config.json",
        "/absolute/path/file.txt",
        "file1.txt",
        "file2.py",
        "component.tsx",
        "file.test.js",
        "config.local.json",
        "Makefile",
        "LICENSE",
        "Dockerfile",
        "CHANGELOG",
        "src/utils/helper.js",
        "docs/api/endpoints.md",
    ];

    for expected in expected_files {
        assert!(
            references.contains(&PathBuf::from(expected)),
            "Expected to find reference: {}",
            expected
        );
    }

    // Should not match email addresses or social handles
    assert!(!references
        .iter()
        .any(|p| p.to_str().unwrap().contains("example.com")));
    assert!(!references
        .iter()
        .any(|p| p.to_str().unwrap().contains("company.org")));
    assert!(!references.iter().any(|p| p.to_str().unwrap() == "username"));
    assert!(!references.iter().any(|p| p.to_str().unwrap() == "user123"));
}

mod read_referenced_files {
    use super::*;

    fn create_ignore_patterns(import_boundary: &Path) -> Gitignore {
        let builder = GitignoreBuilder::new(import_boundary);
        builder.build().unwrap()
    }

    fn create_file(import_boundary: &Path, file_name: &str, content: &str) -> PathBuf {
        let file_path = import_boundary.join(file_name);
        std::fs::write(&file_path, content).unwrap();
        file_path
    }

    fn read_with_budget(
        file_path: &Path,
        import_boundary: &Path,
        ignore_patterns: &Gitignore,
        operations: usize,
        output_bytes: usize,
    ) -> String {
        let mut visited = HashSet::new();
        let mut budget = ExpansionBudget::new(operations, output_bytes);
        read_referenced_files_with_budget(
            &NativeHintFileSystem,
            file_path,
            import_boundary,
            &mut visited,
            0,
            ignore_patterns,
            &mut budget,
        )
    }

    #[test]
    fn test_direct_reference() {
        let temp_dir = tempfile::tempdir().unwrap();
        let import_boundary = temp_dir.path();

        create_file(
            import_boundary,
            "basic_included_file.md",
            "This is basic content",
        );

        let ignore_patterns = create_ignore_patterns(import_boundary);

        let mut visited = HashSet::new();
        let main_file = create_file(
            import_boundary,
            "main.md",
            "Main content\n@basic_included_file.md\nMore content",
        );

        let expanded = read_referenced_files(
            &NativeHintFileSystem,
            &main_file,
            import_boundary,
            &mut visited,
            0,
            &ignore_patterns,
        );

        assert!(expanded.contains("Main content"));
        assert!(expanded.contains("--- Content from"));
        assert!(expanded.contains("This is basic content"));
        assert!(expanded.contains("--- End of"));
        assert!(expanded.contains("More content"));
    }

    #[test]
    fn test_nested_reference() {
        let temp_dir = tempfile::tempdir().unwrap();
        let import_boundary = temp_dir.path();

        create_file(import_boundary, "level1.md", "Level 1 content\n@level2.md");
        create_file(import_boundary, "level2.md", "Level 2 content");

        let mut visited = HashSet::new();
        let main_file = create_file(import_boundary, "main.md", "Main content\n@level1.md");

        let ignore_patterns = create_ignore_patterns(import_boundary);
        let expanded = read_referenced_files(
            &NativeHintFileSystem,
            &main_file,
            import_boundary,
            &mut visited,
            0,
            &ignore_patterns,
        );

        assert!(expanded.contains("Main content"));
        assert!(expanded.contains("Level 1 content"));
        assert!(expanded.contains("Level 2 content"));
    }

    #[test]
    fn test_git_metadata_references_are_not_imported() {
        let temp_dir = tempfile::tempdir().unwrap();
        let import_boundary = temp_dir.path();
        std::fs::create_dir_all(import_boundary.join("docs")).unwrap();
        std::fs::create_dir_all(import_boundary.join("nested/.git")).unwrap();
        std::fs::create_dir_all(import_boundary.join(".github")).unwrap();
        std::fs::create_dir(import_boundary.join(".git")).unwrap();
        create_file(import_boundary, ".git/config", "ROOT_GIT_SECRET");
        create_file(import_boundary, "nested/.git/config", "NESTED_GIT_SECRET");
        create_file(import_boundary, "docs/config.md", "legitimate config");
        create_file(
            import_boundary,
            ".github/instructions.md",
            "legitimate github instructions",
        );
        create_file(import_boundary, ".gitignore", "legitimate gitignore");
        let main_file = create_file(
                import_boundary,
                "main.md",
                "@.git/config\n@docs/../.git/config\n@nested/.git/config\n@docs/config.md\n@.github/instructions.md\n@.gitignore",
            );
        let ignore_patterns = create_ignore_patterns(import_boundary);
        let mut visited = HashSet::new();

        let expanded = read_referenced_files(
            &NativeHintFileSystem,
            &main_file,
            import_boundary,
            &mut visited,
            0,
            &ignore_patterns,
        );

        assert!(!expanded.contains("ROOT_GIT_SECRET"));
        assert!(!expanded.contains("NESTED_GIT_SECRET"));
        assert!(expanded.contains("@.git/config"));
        assert!(expanded.contains("@docs/../.git/config"));
        assert!(expanded.contains("@nested/.git/config"));
        assert!(expanded.contains("legitimate config"));
        assert!(expanded.contains("legitimate github instructions"));
        assert!(expanded.contains("legitimate gitignore"));
    }

    #[test]
    fn test_worktree_git_directories_are_not_imported() {
        let temp_dir = tempfile::tempdir().unwrap();
        let import_boundary = temp_dir.path();
        std::fs::create_dir_all(import_boundary.join(".git-data/worktrees/topic")).unwrap();
        std::fs::create_dir(import_boundary.join(".git-common-data")).unwrap();
        std::fs::create_dir_all(import_boundary.join("project-data")).unwrap();
        create_file(
            import_boundary,
            ".git",
            "gitdir: .git-data/worktrees/topic\n",
        );
        create_file(
            import_boundary,
            ".git-data/worktrees/topic/commondir",
            "../../../.git-common-data\n",
        );
        create_file(
            import_boundary,
            ".git-data/worktrees/topic/config.worktree",
            "WORKTREE_GIT_SECRET",
        );
        create_file(
            import_boundary,
            ".git-common-data/config",
            "COMMON_GIT_SECRET",
        );
        create_file(
            import_boundary,
            "project-data/config.md",
            "legitimate project data",
        );
        let main_file = create_file(
                import_boundary,
                "main.md",
                "@.git-data/worktrees/topic/config.worktree\n@.git-common-data/config\n@project-data/config.md",
            );
        let ignore_patterns = create_ignore_patterns(import_boundary);
        let mut visited = HashSet::new();

        let expanded = read_referenced_files(
            &NativeHintFileSystem,
            &main_file,
            import_boundary,
            &mut visited,
            0,
            &ignore_patterns,
        );

        assert!(!expanded.contains("WORKTREE_GIT_SECRET"));
        assert!(!expanded.contains("COMMON_GIT_SECRET"));
        assert!(expanded.contains("legitimate project data"));
    }

    #[test]
    fn test_nested_worktree_without_gitdir_backpointer_is_not_imported() {
        let temp_dir = tempfile::tempdir().unwrap();
        let import_boundary = temp_dir.path();
        std::fs::create_dir(import_boundary.join("vendor")).unwrap();
        std::fs::create_dir_all(import_boundary.join(".vendor-git/worktrees/topic")).unwrap();
        std::fs::create_dir_all(import_boundary.join(".vendor-common/objects")).unwrap();
        std::fs::create_dir(import_boundary.join(".vendor-common/refs")).unwrap();
        std::fs::create_dir(import_boundary.join(".vendor-git-docs")).unwrap();
        create_file(
            import_boundary,
            "vendor/.git",
            "gitdir: ../.vendor-git/worktrees/topic\n",
        );
        create_file(
            import_boundary,
            ".vendor-git/worktrees/topic/commondir",
            "../../../.vendor-common\n",
        );
        create_file(
            import_boundary,
            ".vendor-git/worktrees/topic/HEAD",
            "ref: refs/heads/topic\n",
        );
        create_file(
            import_boundary,
            ".vendor-git/worktrees/topic/config.worktree",
            "NESTED_WORKTREE_GIT_SECRET",
        );
        create_file(
            import_boundary,
            ".vendor-common/config",
            "NESTED_COMMON_GIT_SECRET",
        );
        create_file(
            import_boundary,
            ".vendor-common/HEAD",
            "ref: refs/heads/main\n",
        );
        create_file(
            import_boundary,
            ".vendor-git-docs/config.md",
            "legitimate similarly named data",
        );
        let main_file = create_file(
                import_boundary,
                "main.md",
                "@.vendor-git/worktrees/topic/config.worktree\n@.vendor-common/config\n@.vendor-git-docs/config.md",
            );
        let mut builder = GitignoreBuilder::new(import_boundary);
        builder.add_line(None, "vendor/").unwrap();
        let ignore_patterns = builder.build().unwrap();
        let mut visited = HashSet::new();

        let expanded = read_referenced_files(
            &NativeHintFileSystem,
            &main_file,
            import_boundary,
            &mut visited,
            0,
            &ignore_patterns,
        );

        assert!(!expanded.contains("NESTED_WORKTREE_GIT_SECRET"));
        assert!(!expanded.contains("NESTED_COMMON_GIT_SECRET"));
        assert!(expanded.contains("legitimate similarly named data"));
    }

    #[cfg(unix)]
    #[test]
    fn test_nested_worktree_with_symlinked_commondir_is_not_imported() {
        use std::os::unix::fs::symlink;

        let temp_dir = tempfile::tempdir().unwrap();
        let import_boundary = temp_dir.path();
        std::fs::create_dir_all(import_boundary.join(".vendor-git/worktrees/topic")).unwrap();
        create_file(
            import_boundary,
            ".vendor-git/worktrees/topic/HEAD",
            "ref: refs/heads/topic\n",
        );
        create_file(
            import_boundary,
            ".vendor-git/worktrees/topic/config.worktree",
            "NESTED_WORKTREE_GIT_SECRET",
        );
        create_file(
            import_boundary,
            "commondir-marker",
            "../../../.vendor-common\n",
        );
        symlink(
            "../../../commondir-marker",
            import_boundary.join(".vendor-git/worktrees/topic/commondir"),
        )
        .unwrap();
        let main_file = create_file(
            import_boundary,
            "main.md",
            "@.vendor-git/worktrees/topic/config.worktree",
        );
        let ignore_patterns = create_ignore_patterns(import_boundary);
        let mut visited = HashSet::new();

        let expanded = read_referenced_files(
            &NativeHintFileSystem,
            &main_file,
            import_boundary,
            &mut visited,
            0,
            &ignore_patterns,
        );

        assert_eq!(expanded, "@.vendor-git/worktrees/topic/config.worktree");
    }

    #[cfg(unix)]
    #[test]
    fn test_symlinked_hint_resolves_references_from_symlink_directory() {
        use std::os::unix::fs::symlink;

        let temp_dir = tempfile::tempdir().unwrap();
        let import_boundary = temp_dir.path();
        std::fs::create_dir(import_boundary.join("docs")).unwrap();
        create_file(import_boundary, "root-only.md", "ROOT_RELATIVE_CONTENT");
        create_file(
            import_boundary,
            "docs/root-only.md",
            "TARGET_RELATIVE_CONTENT",
        );
        create_file(import_boundary, "docs/shared.md", "@root-only.md");
        symlink("docs/shared.md", import_boundary.join("AGENTS.md")).unwrap();
        let ignore_patterns = create_ignore_patterns(import_boundary);
        let mut visited = HashSet::new();

        let expanded = read_referenced_files(
            &NativeHintFileSystem,
            &import_boundary.join("AGENTS.md"),
            import_boundary,
            &mut visited,
            0,
            &ignore_patterns,
        );

        assert!(expanded.contains("ROOT_RELATIVE_CONTENT"));
        assert!(!expanded.contains("TARGET_RELATIVE_CONTENT"));
    }

    #[cfg(unix)]
    #[test]
    fn test_final_git_metadata_symlinks_are_not_imported() {
        use std::os::unix::fs::symlink;

        let temp_dir = tempfile::tempdir().unwrap();
        let import_boundary = temp_dir.path();
        std::fs::create_dir(import_boundary.join(".git-data")).unwrap();
        create_file(import_boundary, ".git", "gitdir: .git-data\n");
        create_file(import_boundary, "ordinary.md", "ALIASED_GIT_SECRET");
        symlink("../ordinary.md", import_boundary.join(".git-data/config")).unwrap();
        let main_file = create_file(import_boundary, "main.md", "@.git-data/config");
        let ignore_patterns = create_ignore_patterns(import_boundary);
        let mut visited = HashSet::new();

        let expanded = read_referenced_files(
            &NativeHintFileSystem,
            &main_file,
            import_boundary,
            &mut visited,
            0,
            &ignore_patterns,
        );
        let mut root_visited = HashSet::new();
        let aliased_root = read_referenced_files(
            &NativeHintFileSystem,
            &import_boundary.join(".git-data/config"),
            import_boundary,
            &mut root_visited,
            0,
            &ignore_patterns,
        );

        assert_eq!(expanded, "@.git-data/config");
        assert!(aliased_root.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn test_structural_git_detection_follows_supported_marker_symlinks() {
        use std::os::unix::fs::symlink;

        let temp_dir = tempfile::tempdir().unwrap();
        let import_boundary = temp_dir.path();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(import_boundary.join("nested-git")).unwrap();
        std::fs::create_dir_all(import_boundary.join("project-data/objects")).unwrap();
        std::fs::create_dir(outside.path().join("objects")).unwrap();
        std::fs::create_dir(outside.path().join("refs")).unwrap();
        std::fs::write(outside.path().join("HEAD"), "ref: refs/heads/main\n").unwrap();
        create_file(import_boundary, "nested-git/config", "NESTED_GIT_SECRET");
        create_file(
            import_boundary,
            "project-data/config.md",
            "legitimate project data",
        );
        symlink(
            outside.path().join("HEAD"),
            import_boundary.join("nested-git/HEAD"),
        )
        .unwrap();
        symlink(
            outside.path().join("objects"),
            import_boundary.join("nested-git/objects"),
        )
        .unwrap();
        symlink(
            outside.path().join("refs"),
            import_boundary.join("nested-git/refs"),
        )
        .unwrap();
        symlink(
            outside.path().join("HEAD"),
            import_boundary.join("project-data/HEAD"),
        )
        .unwrap();
        let main_file = create_file(
            import_boundary,
            "main.md",
            "@nested-git/config\n@project-data/config.md",
        );
        let ignore_patterns = create_ignore_patterns(import_boundary);
        let mut visited = HashSet::new();

        let expanded = read_referenced_files(
            &NativeHintFileSystem,
            &main_file,
            import_boundary,
            &mut visited,
            0,
            &ignore_patterns,
        );

        assert!(!expanded.contains("NESTED_GIT_SECRET"));
        assert!(expanded.contains("legitimate project data"));
    }

    #[cfg(unix)]
    #[test]
    fn test_unborn_git_directory_with_dangling_head_symlink_is_not_imported() {
        use std::os::unix::fs::symlink;

        let temp_dir = tempfile::tempdir().unwrap();
        let import_boundary = temp_dir.path();
        std::fs::create_dir_all(import_boundary.join(".vendor-git/objects")).unwrap();
        std::fs::create_dir_all(import_boundary.join(".vendor-git/refs/heads")).unwrap();
        create_file(import_boundary, ".vendor-git/config", "UNBORN_GIT_SECRET");
        symlink("refs/heads/topic", import_boundary.join(".vendor-git/HEAD")).unwrap();
        let main_file = create_file(import_boundary, "main.md", "@.vendor-git/config");
        let ignore_patterns = create_ignore_patterns(import_boundary);
        let mut visited = HashSet::new();

        let expanded = read_referenced_files(
            &NativeHintFileSystem,
            &main_file,
            import_boundary,
            &mut visited,
            0,
            &ignore_patterns,
        );

        assert_eq!(expanded, "@.vendor-git/config");
    }

    #[cfg(unix)]
    #[test]
    fn test_symlink_aliases_into_git_metadata_are_not_imported() {
        use std::os::unix::fs::symlink;

        let temp_dir = tempfile::tempdir().unwrap();
        let import_boundary = temp_dir.path();
        let git_dir = import_boundary.join(".git");
        std::fs::create_dir(&git_dir).unwrap();
        create_file(import_boundary, ".git/config", "ALIASED_GIT_SECRET");
        symlink(git_dir.join("config"), import_boundary.join("metadata.md")).unwrap();
        let main_file = create_file(import_boundary, "main.md", "@metadata.md");
        let ignore_patterns = create_ignore_patterns(import_boundary);
        let mut visited = HashSet::new();

        let expanded = read_referenced_files(
            &NativeHintFileSystem,
            &main_file,
            import_boundary,
            &mut visited,
            0,
            &ignore_patterns,
        );
        let mut root_visited = HashSet::new();
        let aliased_root = read_referenced_files(
            &NativeHintFileSystem,
            &import_boundary.join("metadata.md"),
            import_boundary,
            &mut root_visited,
            0,
            &ignore_patterns,
        );

        assert_eq!(expanded, "@metadata.md");
        assert!(aliased_root.is_empty());
    }

    #[test]
    fn test_reference_operation_budget_preserves_excess_references() {
        let temp_dir = tempfile::tempdir().unwrap();
        let import_boundary = temp_dir.path();
        let ignore_patterns = create_ignore_patterns(import_boundary);
        let mut references = Vec::new();

        for index in 0..65 {
            let file_name = format!("included_{index}.md");
            create_file(
                import_boundary,
                &file_name,
                &format!("included content {index}"),
            );
            references.push(format!("@{file_name}"));
        }

        let main_file = create_file(import_boundary, "main.md", &references.join("\n"));
        let mut visited = HashSet::new();
        let expanded = read_referenced_files(
            &NativeHintFileSystem,
            &main_file,
            import_boundary,
            &mut visited,
            0,
            &ignore_patterns,
        );

        assert!(expanded.contains("included content 63"));
        assert!(expanded.contains("@included_64.md"));
        assert!(!expanded.contains("included content 64"));
    }

    #[test]
    fn test_expanded_output_budget_preserves_excess_references() {
        let temp_dir = tempfile::tempdir().unwrap();
        let import_boundary = temp_dir.path();
        let ignore_patterns = create_ignore_patterns(import_boundary);
        let included_content = "x".repeat(131_072);
        let mut references = Vec::new();

        for index in 0..9 {
            let file_name = format!("included_{index}.md");
            create_file(import_boundary, &file_name, &included_content);
            references.push(format!("@{file_name}"));
        }

        let main_file = create_file(import_boundary, "main.md", &references.join("\n"));
        let mut visited = HashSet::new();
        let expanded = read_referenced_files(
            &NativeHintFileSystem,
            &main_file,
            import_boundary,
            &mut visited,
            0,
            &ignore_patterns,
        );

        assert!(expanded.contains("@included_8.md"));
        assert!(expanded.len() <= references.join("\n").len() + 1_048_576);
    }

    #[test]
    fn test_repeated_references_share_operation_budget() {
        let temp_dir = tempfile::tempdir().unwrap();
        let import_boundary = temp_dir.path();
        let ignore_patterns = create_ignore_patterns(import_boundary);
        create_file(import_boundary, "shared.md", "shared content");
        let main_file = create_file(
            import_boundary,
            "main.md",
            "@shared.md\n@shared.md\n@shared.md",
        );

        let expanded = read_with_budget(
            &main_file,
            import_boundary,
            &ignore_patterns,
            2,
            MAX_EXPANDED_OUTPUT_BYTES,
        );

        assert_eq!(expanded.matches("shared content").count(), 2);
        assert_eq!(expanded.matches("@shared.md").count(), 1);
    }

    #[test]
    fn test_branching_references_share_operation_budget() {
        let temp_dir = tempfile::tempdir().unwrap();
        let import_boundary = temp_dir.path();
        let ignore_patterns = create_ignore_patterns(import_boundary);
        create_file(import_boundary, "leaf1.md", "leaf one");
        create_file(import_boundary, "leaf2.md", "leaf two");
        create_file(import_boundary, "leaf3.md", "leaf three");
        create_file(import_boundary, "branch1.md", "@leaf1.md\n@leaf2.md");
        create_file(import_boundary, "branch2.md", "@leaf3.md");
        let main_file = create_file(import_boundary, "main.md", "@branch1.md\n@branch2.md");

        let expanded = read_with_budget(
            &main_file,
            import_boundary,
            &ignore_patterns,
            3,
            MAX_EXPANDED_OUTPUT_BYTES,
        );

        assert!(expanded.contains("leaf one"));
        assert!(expanded.contains("leaf two"));
        assert!(expanded.contains("@branch2.md"));
        assert!(!expanded.contains("leaf three"));
    }

    #[test]
    fn test_output_budget_boundary_and_exhaustion() {
        let temp_dir = tempfile::tempdir().unwrap();
        let import_boundary = temp_dir.path();
        let ignore_patterns = create_ignore_patterns(import_boundary);
        let included_content = "included content";
        create_file(import_boundary, "included.md", included_content);
        create_file(import_boundary, "later.md", "later content");
        let main_file = create_file(import_boundary, "main.md", "@included.md\n@later.md");
        let exact_cost =
            expanded_output_cost(Path::new("included.md"), included_content.len()).unwrap();

        let at_boundary = read_with_budget(
            &main_file,
            import_boundary,
            &ignore_patterns,
            MAX_REFERENCE_OPERATIONS,
            exact_cost,
        );
        let below_boundary = read_with_budget(
            &main_file,
            import_boundary,
            &ignore_patterns,
            MAX_REFERENCE_OPERATIONS,
            exact_cost - 1,
        );

        assert!(at_boundary.contains("included content"));
        assert!(at_boundary.contains("@later.md"));
        assert!(!at_boundary.contains("later content"));
        assert!(below_boundary.contains("@included.md"));
        assert!(below_boundary.contains("@later.md"));
        assert!(!below_boundary.contains("included content"));
    }

    #[test]
    fn test_circular_reference() {
        let temp_dir = tempfile::tempdir().unwrap();
        let import_boundary = temp_dir.path();

        let ignore_patterns = create_ignore_patterns(import_boundary);
        create_file(import_boundary, "file1.md", "File 1\n@file2.md");
        create_file(import_boundary, "file2.md", "File 2\n@file1.md");
        let main_file = create_file(import_boundary, "main.md", "Main\n@file1.md");

        let mut visited = HashSet::new();
        let expanded = read_referenced_files(
            &NativeHintFileSystem,
            &main_file,
            import_boundary,
            &mut visited,
            0,
            &ignore_patterns,
        );

        assert!(expanded.contains("File 1"));
        assert!(expanded.contains("File 2"));
        // Should only appear once due to circular reference protection
        let file1_count = expanded.matches("File 1").count();
        assert_eq!(file1_count, 1);
    }

    #[test]
    fn test_max_depth_limit() {
        let temp_dir = tempfile::tempdir().unwrap();
        let import_boundary = temp_dir.path();
        let ignore_patterns = create_ignore_patterns(import_boundary);
        let mut visited = HashSet::new();
        for i in 1..=5 {
            let content = if i < 5 {
                format!("Level {} content\n@level{}.md", i, i + 1)
            } else {
                format!("Level {} content", i)
            };
            create_file(import_boundary, &format!("level{}.md", i), &content);
        }
        let main_file = create_file(import_boundary, "main.md", "Main\n@level1.md");
        let expanded = read_referenced_files(
            &NativeHintFileSystem,
            &main_file,
            import_boundary,
            &mut visited,
            0,
            &ignore_patterns,
        );
        // Should contain up to level 3 (MAX_DEPTH = 3)
        assert!(expanded.contains("Level 1 content"));
        assert!(expanded.contains("Level 2 content"));
        assert!(expanded.contains("Level 3 content"));
        // Should not contain level 4 or 5 due to depth limit
        assert!(!expanded.contains("Level 4 content"));
        assert!(!expanded.contains("Level 5 content"));
    }

    #[test]
    fn test_missing_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let import_boundary = temp_dir.path();
        let ignore_patterns = create_ignore_patterns(import_boundary);
        let mut visited = HashSet::new();
        let main_file = create_file(
            import_boundary,
            "main.md",
            "Main\n@missing.md\nMore content",
        );

        let expanded = read_referenced_files(
            &NativeHintFileSystem,
            &main_file,
            import_boundary,
            &mut visited,
            0,
            &ignore_patterns,
        );

        assert!(expanded.contains("@missing.md"));
        assert!(!expanded.contains("--- Content from"));
    }

    #[test]
    fn test_read_referenced_files_respects_ignore() {
        let temp_dir = tempfile::tempdir().unwrap();
        let import_boundary = temp_dir.path();

        create_file(import_boundary, "allowed.md", "Allowed content");
        create_file(import_boundary, "secret.md", "Secret content");

        let mut builder = GitignoreBuilder::new(import_boundary);
        builder.add_line(None, "secret.md").unwrap();
        let ignore_patterns = builder.build().unwrap();

        let mut visited = HashSet::new();
        // Create main content with references
        let content = "Main\n@allowed.md\n@secret.md";
        let main_file = create_file(import_boundary, "main.md", content);
        let expanded = read_referenced_files(
            &NativeHintFileSystem,
            &main_file,
            import_boundary,
            &mut visited,
            0,
            &ignore_patterns,
        );

        // Should contain allowed content but not ignored content
        assert!(expanded.contains("Allowed content"));
        assert!(!expanded.contains("Secret content"));

        // The @secret.md reference should remain unchanged
        assert!(expanded.contains("@secret.md"));

        temp_dir.close().unwrap();
    }

    #[test]
    fn test_security_integration_with_file_expansion() {
        let temp_dir = tempfile::tempdir().unwrap();
        let import_boundary = temp_dir.path();
        let ignore_patterns = create_ignore_patterns(import_boundary);

        // Create a legitimate file
        create_file(
            import_boundary,
            "legitimate_file.md",
            "This is safe content",
        );

        let absolute_path_file = create_file(
            import_boundary,
            "used_with_absolute_path.md",
            "Absolute path content",
        );
        let absolute_path_file_path = absolute_path_file
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();

        // Create a config file attempting path traversal
        let malicious_content = format!(
            r#"
            Normal content here.
            @../etc/passwd
            @{}
            @legitimate_file.md
            "#,
            absolute_path_file_path
        );
        create_file(import_boundary, "main.md", &malicious_content);

        let mut visited = HashSet::new();
        let expanded = read_referenced_files(
            &NativeHintFileSystem,
            &import_boundary.join("main.md"),
            import_boundary,
            &mut visited,
            0,
            &ignore_patterns,
        );

        // Should contain the legitimate file but not the malicious attempts
        assert!(expanded.contains("This is safe content"));
        assert!(!expanded.contains("root:")); // Common content in /etc/passwd
        assert!(!expanded.contains("Absolute path content"));

        // The malicious references should still be present (not expanded)
        assert!(expanded.contains("@../etc/passwd"));
        assert!(expanded.contains(absolute_path_file_path.as_str()));
    }
}
