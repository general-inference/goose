use super::filesystem::HintFileSystem;
use ignore::gitignore::Gitignore;
use std::sync::LazyLock;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

static FILE_REFERENCE_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?:^|\s)@([a-zA-Z0-9_\-./]+(?:\.[a-zA-Z0-9]+)+|[A-Z][a-zA-Z0-9_\-]*|[a-zA-Z0-9_\-./]*[./][a-zA-Z0-9_\-./]*)")
        .expect("Invalid file reference regex pattern")
});

const MAX_DEPTH: usize = 3;
const MAX_REFERENCE_OPERATIONS: usize = 64;
const MAX_EXPANDED_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_GIT_POINTER_BYTES: u64 = 4096;

struct FileReference {
    path: PathBuf,
    start: usize,
    end: usize,
}

struct ExpansionBudget {
    remaining_operations: usize,
    remaining_output_bytes: usize,
    exhausted: bool,
}

struct ImportBoundary<'a> {
    fs: &'a dyn HintFileSystem,
    canonical: PathBuf,
    git_metadata_directories: Vec<PathBuf>,
}

impl ExpansionBudget {
    fn new(operations: usize, output_bytes: usize) -> Self {
        Self {
            remaining_operations: operations,
            remaining_output_bytes: output_bytes,
            exhausted: false,
        }
    }

    fn consume_operation(&mut self) -> bool {
        if self.exhausted || self.remaining_operations == 0 {
            self.exhausted = true;
            return false;
        }

        self.remaining_operations -= 1;
        true
    }

    fn reserve_output(&mut self, bytes: usize) -> bool {
        if self.exhausted || bytes > self.remaining_output_bytes {
            self.exhausted = true;
            return false;
        }

        self.remaining_output_bytes -= bytes;
        if self.remaining_output_bytes == 0 {
            self.exhausted = true;
        }
        true
    }

    fn can_fit_output(&mut self, bytes: usize) -> bool {
        if self.exhausted || bytes > self.remaining_output_bytes {
            self.exhausted = true;
            return false;
        }

        true
    }
}

fn contains_git_metadata_component(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|component| component.eq_ignore_ascii_case(".git"))
    })
}

fn canonical_git_directory(fs: &dyn HintFileSystem, path: PathBuf) -> Option<PathBuf> {
    fs.canonicalize(&path).ok().filter(|path| fs.is_dir(path))
}

fn resolve_git_path(fs: &dyn HintFileSystem, base: &Path, value: &str) -> Option<PathBuf> {
    let value = value.lines().next()?.trim();
    if value.is_empty() {
        return None;
    }
    let path = Path::new(value);
    canonical_git_directory(
        fs,
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            base.join(path)
        },
    )
}

fn read_git_pointer(fs: &dyn HintFileSystem, path: &Path) -> Option<String> {
    let metadata = fs.symlink_metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    fs.read_text(path, MAX_GIT_POINTER_BYTES, false).ok()
}

fn git_metadata_directories(fs: &dyn HintFileSystem, boundary_canonical: &Path) -> Vec<PathBuf> {
    let dot_git = boundary_canonical.join(".git");
    let git_dir = if fs
        .symlink_metadata(&dot_git)
        .ok()
        .is_some_and(|metadata| metadata.is_file())
    {
        read_git_pointer(fs, &dot_git).and_then(|contents| {
            contents
                .strip_prefix("gitdir:")
                .and_then(|value| resolve_git_path(fs, boundary_canonical, value))
        })
    } else {
        canonical_git_directory(fs, dot_git)
    };
    let Some(git_dir) = git_dir else {
        return Vec::new();
    };

    let mut directories = vec![git_dir.clone()];
    if let Some(common_dir) = read_git_pointer(fs, &git_dir.join("commondir"))
        .and_then(|value| resolve_git_path(fs, &git_dir, &value))
    {
        if common_dir != git_dir {
            directories.push(common_dir);
        }
    }
    directories
}

fn is_regular_file_following_symlinks(fs: &dyn HintFileSystem, path: &Path) -> bool {
    fs.metadata(path)
        .ok()
        .is_some_and(|metadata| metadata.is_file())
}

fn is_regular_file_or_symlink(fs: &dyn HintFileSystem, path: &Path) -> bool {
    fs.symlink_metadata(path)
        .ok()
        .is_some_and(|metadata| metadata.is_file() || metadata.is_symlink())
}

fn is_directory_following_symlinks(fs: &dyn HintFileSystem, path: &Path) -> bool {
    fs.metadata(path)
        .ok()
        .is_some_and(|metadata| metadata.is_dir())
}

fn is_structural_git_directory(fs: &dyn HintFileSystem, path: &Path) -> bool {
    is_regular_file_or_symlink(fs, &path.join("HEAD"))
        && ((is_directory_following_symlinks(fs, &path.join("objects"))
            && is_directory_following_symlinks(fs, &path.join("refs")))
            || is_regular_file_following_symlinks(fs, &path.join("commondir")))
}

fn has_structural_git_ancestor(
    fs: &dyn HintFileSystem,
    canonical: &Path,
    boundary_canonical: &Path,
) -> bool {
    canonical
        .ancestors()
        .take_while(|ancestor| ancestor.starts_with(boundary_canonical))
        .any(|path| is_structural_git_directory(fs, path))
}

impl<'a> ImportBoundary<'a> {
    fn new(fs: &'a dyn HintFileSystem, import_boundary: &Path) -> Result<Self, std::io::Error> {
        let canonical = canonical_import_boundary(fs, import_boundary)?;
        let git_metadata_directories = git_metadata_directories(fs, &canonical);
        Ok(Self {
            fs,
            canonical,
            git_metadata_directories,
        })
    }
}

fn validate_canonical_path(
    canonical: PathBuf,
    import_boundary: &ImportBoundary,
    original: &Path,
) -> Result<PathBuf, std::io::Error> {
    let fs = import_boundary.fs;
    let relative = canonical
        .strip_prefix(&import_boundary.canonical)
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "Include: '{}' is outside the import boundary '{}'",
                    original.display(),
                    import_boundary.canonical.display()
                ),
            )
        })?;
    if contains_git_metadata_component(relative)
        || import_boundary
            .git_metadata_directories
            .iter()
            .any(|directory| canonical.starts_with(directory))
        || has_structural_git_ancestor(fs, &canonical, &import_boundary.canonical)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("Git metadata path not allowed: '{}'", original.display()),
        ));
    }
    Ok(canonical)
}

fn canonical_import_boundary(
    fs: &dyn HintFileSystem,
    import_boundary: &Path,
) -> Result<PathBuf, std::io::Error> {
    fs.canonicalize(import_boundary).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Import boundary directory not found",
        )
    })
}

fn validate_canonical_parent(
    path: &Path,
    import_boundary: &ImportBoundary,
) -> Result<(), std::io::Error> {
    let fs = import_boundary.fs;
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let canonical_parent = fs.canonicalize(parent)?;
    validate_canonical_path(canonical_parent, import_boundary, parent).map(|_| ())
}

fn sanitize_existing_path(
    path: &Path,
    import_boundary: &ImportBoundary,
) -> Result<PathBuf, std::io::Error> {
    let fs = import_boundary.fs;
    validate_canonical_parent(path, import_boundary)?;
    let canonical = fs.canonicalize(path)?;
    validate_canonical_path(canonical, import_boundary, path)
}

fn sanitize_reference_path(
    reference: &Path,
    including_file_path: &Path,
    import_boundary: &ImportBoundary,
) -> Result<PathBuf, std::io::Error> {
    let fs = import_boundary.fs;
    if reference.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "Absolute paths not allowed in file references",
        ));
    }
    if contains_git_metadata_component(reference) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("Git metadata path not allowed: '{}'", reference.display()),
        ));
    }
    let resolved = including_file_path.join(reference);
    match validate_canonical_parent(&resolved, import_boundary) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(resolved),
        Err(error) => return Err(error),
    }

    match fs.canonicalize(&resolved) {
        Ok(canonical) => validate_canonical_path(canonical, import_boundary, &resolved),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(resolved),
        Err(error) => Err(error),
    }
}

fn find_file_references(content: &str) -> Vec<FileReference> {
    // Keep size limits for ReDoS protection - .goosehints should be reasonably sized
    const MAX_CONTENT_LENGTH: usize = 131_072; // 128KB limit

    if content.len() > MAX_CONTENT_LENGTH {
        tracing::warn!(
            "Content too large for file reference parsing: {} bytes (limit: {} bytes)",
            content.len(),
            MAX_CONTENT_LENGTH
        );
        return Vec::new();
    }

    FILE_REFERENCE_REGEX
        .captures_iter(content)
        .filter_map(|captures| {
            let path_match = captures.get(1)?;
            Some(FileReference {
                path: PathBuf::from(path_match.as_str()),
                start: path_match.start().checked_sub(1)?,
                end: path_match.end(),
            })
        })
        .collect()
}

#[cfg(test)]
fn parse_file_references(content: &str) -> Vec<PathBuf> {
    find_file_references(content)
        .into_iter()
        .map(|reference| reference.path)
        .collect()
}

fn expanded_output_cost(reference: &Path, content_bytes: usize) -> Option<usize> {
    let reference_display = reference.to_string_lossy();
    let wrapper_bytes = format!(
        "--- Content from {} ---\n\n--- End of {} ---",
        reference_display, reference_display
    )
    .len();
    content_bytes.checked_add(wrapper_bytes)
}

fn content_between(content: &str, start: usize, end: usize) -> &str {
    content
        .get(start..end)
        .expect("regex match offsets must be UTF-8 boundaries")
}

fn should_process_reference(
    reference: &Path,
    including_file_path: &Path,
    import_boundary: &ImportBoundary,
    visited: &HashSet<PathBuf>,
    ignore_patterns: &Gitignore,
) -> Option<PathBuf> {
    let fs = import_boundary.fs;
    if visited.contains(reference) {
        return None;
    }
    let safe_path = match sanitize_reference_path(reference, including_file_path, import_boundary) {
        Ok(path) => path,
        Err(_) => {
            tracing::warn!("Skipping unsafe file reference: {:?}", reference);
            return None;
        }
    };

    if ignore_patterns.matched(&safe_path, false).is_ignore() {
        tracing::debug!("Skipping ignored file reference: {:?}", safe_path);
        return None;
    }

    if !fs.is_file(&safe_path) {
        return None;
    }

    Some(safe_path)
}

fn process_file_reference(
    reference: &Path,
    safe_path: &Path,
    visited: &mut HashSet<PathBuf>,
    import_boundary: &ImportBoundary,
    depth: usize,
    ignore_patterns: &Gitignore,
    budget: &mut ExpansionBudget,
) -> Option<String> {
    let fs = import_boundary.fs;
    let wrapper_bytes = expanded_output_cost(reference, 0)?;
    if !budget.can_fit_output(wrapper_bytes) {
        return None;
    }

    let file_size = usize::try_from(fs.metadata(safe_path).ok()?.len()).ok()?;
    let estimated_output = expanded_output_cost(reference, file_size)?;
    if !budget.can_fit_output(estimated_output) {
        return None;
    }

    let max_content_bytes = budget.remaining_output_bytes - wrapper_bytes;
    let content = match fs.read_text(safe_path, u64::try_from(max_content_bytes).ok()?, true) {
        Ok(content) => content,
        Err(e) => {
            tracing::warn!("Could not read file {:?}: {}", safe_path, e);
            return None;
        }
    };

    let output_bytes = expanded_output_cost(reference, content.len())?;
    if !budget.reserve_output(output_bytes) {
        return None;
    }

    visited.insert(reference.to_path_buf());

    let expanded_content = expand_file_content(
        &content,
        safe_path,
        import_boundary,
        visited,
        depth + 1,
        ignore_patterns,
        budget,
    );

    let replacement = format!(
        "--- Content from {} ---\n{}\n--- End of {} ---",
        reference.display(),
        expanded_content,
        reference.display()
    );

    visited.remove(reference);

    Some(replacement)
}

fn expand_file_content(
    content: &str,
    file_path: &Path,
    import_boundary: &ImportBoundary,
    visited: &mut HashSet<PathBuf>,
    depth: usize,
    ignore_patterns: &Gitignore,
    budget: &mut ExpansionBudget,
) -> String {
    let including_file_path = file_path.parent().unwrap_or(file_path);
    let references = find_file_references(content);
    let mut result = String::with_capacity(content.len());
    let mut cursor = 0;

    for reference in references {
        result.push_str(content_between(content, cursor, reference.start));
        cursor = reference.end;

        if depth >= MAX_DEPTH || !budget.consume_operation() {
            result.push_str(content_between(content, reference.start, reference.end));
            continue;
        }

        let safe_path = match should_process_reference(
            &reference.path,
            including_file_path,
            import_boundary,
            visited,
            ignore_patterns,
        ) {
            Some(path) => path,
            None => {
                result.push_str(content_between(content, reference.start, reference.end));
                continue;
            }
        };

        if let Some(replacement) = process_file_reference(
            &reference.path,
            &safe_path,
            visited,
            import_boundary,
            depth,
            ignore_patterns,
            budget,
        ) {
            result.push_str(&replacement);
        } else {
            result.push_str(content_between(content, reference.start, reference.end));
        }
    }

    result.push_str(content_between(content, cursor, content.len()));
    result
}

fn read_referenced_files_with_budget(
    fs: &dyn HintFileSystem,
    file_path: &Path,
    import_boundary: &Path,
    visited: &mut HashSet<PathBuf>,
    depth: usize,
    ignore_patterns: &Gitignore,
    budget: &mut ExpansionBudget,
) -> String {
    let import_boundary = match ImportBoundary::new(fs, import_boundary) {
        Ok(import_boundary) => import_boundary,
        Err(e) => {
            tracing::warn!("Skipping unsafe hint file {:?}: {}", file_path, e);
            return String::new();
        }
    };
    let safe_file_path = match sanitize_existing_path(file_path, &import_boundary) {
        Ok(path) => path,
        Err(e) => {
            tracing::warn!("Skipping unsafe hint file {:?}: {}", file_path, e);
            return String::new();
        }
    };
    let content = match fs.read_text(&safe_file_path, u64::MAX, true) {
        Ok(content) => content,
        Err(e) => {
            tracing::warn!("Could not read file {:?}: {}", safe_file_path, e);
            return String::new();
        }
    };

    expand_file_content(
        &content,
        file_path,
        &import_boundary,
        visited,
        depth,
        ignore_patterns,
        budget,
    )
}

pub fn read_referenced_files(
    fs: &dyn HintFileSystem,
    file_path: &Path,
    import_boundary: &Path,
    visited: &mut HashSet<PathBuf>,
    depth: usize,
    ignore_patterns: &Gitignore,
) -> String {
    let mut budget = ExpansionBudget::new(MAX_REFERENCE_OPERATIONS, MAX_EXPANDED_OUTPUT_BYTES);
    read_referenced_files_with_budget(
        fs,
        file_path,
        import_boundary,
        visited,
        depth,
        ignore_patterns,
        &mut budget,
    )
}

#[cfg(test)]
#[path = "import_tests.rs"]
mod tests;
