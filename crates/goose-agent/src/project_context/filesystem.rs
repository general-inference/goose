use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// File properties needed for instruction discovery and Git metadata exclusion.
#[derive(Clone, Copy, Debug)]
pub struct HintMetadata {
    pub is_file: bool,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub length: u64,
}

impl HintMetadata {
    pub fn is_file(&self) -> bool {
        self.is_file
    }
    pub fn is_dir(&self) -> bool {
        self.is_dir
    }
    pub fn is_symlink(&self) -> bool {
        self.is_symlink
    }
    pub fn len(&self) -> u64 {
        self.length
    }
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }
}

/// All access used by hint discovery, including imported files and .gitignore.
///
/// A restricted host must enforce its policy during each operation, even when
/// paths have already been canonicalized. Those checks cannot prevent races.
pub trait HintFileSystem: Send + Sync {
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf>;
    fn metadata(&self, path: &Path) -> io::Result<HintMetadata>;
    fn symlink_metadata(&self, path: &Path) -> io::Result<HintMetadata>;
    /// Read a regular UTF-8 file, rejecting content larger than `limit`.
    /// When `follow_symlinks` is false, reject a final-component symlink.
    /// Hosts may impose smaller limits. Do not block indefinitely on FIFOs.
    fn read_text(&self, path: &Path, limit: u64, follow_symlinks: bool) -> io::Result<String>;
    fn is_file(&self, path: &Path) -> bool {
        self.metadata(path).is_ok_and(|metadata| metadata.is_file())
    }
    fn is_dir(&self, path: &Path) -> bool {
        self.metadata(path).is_ok_and(|metadata| metadata.is_dir())
    }
}

/// Goose's native filesystem access. This backend does not provide confinement.
#[derive(Clone, Copy, Debug, Default)]
pub struct NativeHintFileSystem;

impl From<std::fs::Metadata> for HintMetadata {
    fn from(metadata: std::fs::Metadata) -> Self {
        Self {
            is_file: metadata.is_file(),
            is_dir: metadata.is_dir(),
            is_symlink: metadata.file_type().is_symlink(),
            length: metadata.len(),
        }
    }
}

impl HintFileSystem for NativeHintFileSystem {
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        path.canonicalize()
    }
    fn metadata(&self, path: &Path) -> io::Result<HintMetadata> {
        std::fs::metadata(path).map(Into::into)
    }
    fn symlink_metadata(&self, path: &Path) -> io::Result<HintMetadata> {
        std::fs::symlink_metadata(path).map(Into::into)
    }
    fn read_text(&self, path: &Path, limit: u64, follow_symlinks: bool) -> io::Result<String> {
        if !follow_symlinks && self.symlink_metadata(path)?.is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "symlink not allowed",
            ));
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(
                libc::O_NONBLOCK | if follow_symlinks { 0 } else { libc::O_NOFOLLOW },
            );
        }
        #[cfg(windows)]
        if !follow_symlinks {
            use std::os::windows::fs::OpenOptionsExt;
            // Inspect the opened reparse point itself, rather than racing a pre-check.
            const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
            options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let file = options.open(path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected a regular file",
            ));
        }
        let mut content = String::new();
        file.take(limit.saturating_add(1))
            .read_to_string(&mut content)?;
        if content.len() as u64 > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "instruction file exceeds read limit",
            ));
        }
        Ok(content)
    }
}
