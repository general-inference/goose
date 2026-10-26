//! Filesystem operations used by the shared developer tools.
use std::{ffi::OsString, io, io::Read, path::Path};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    File,
    Directory,
    Symlink,
    Other,
}

pub struct DirectoryEntry {
    pub name: OsString,
    pub kind: FileKind,
}

/// Hosts enforce access during each operation, including symlink resolution.
/// A path check followed by unrestricted I/O does not provide confinement.
pub trait FileSystem: Send + Sync {
    fn read_to_string(&self, path: &Path) -> io::Result<String>;
    fn write(&self, path: &Path, content: &str) -> io::Result<()>;
    fn create_dir_all(&self, path: &Path) -> io::Result<()>;
    fn exists(&self, path: &Path) -> bool;

    /// Returns at most `limit + 1` bytes, allowing the caller to detect overflow.
    fn read(&self, _path: &Path, _limit: u64) -> io::Result<Vec<u8>> {
        Err(io::ErrorKind::Unsupported.into())
    }

    /// Entry kinds describe the entry itself, without following symlinks.
    fn read_dir(&self, _path: &Path) -> io::Result<Vec<DirectoryEntry>> {
        Err(io::ErrorKind::Unsupported.into())
    }

    /// Hosts may omit ignore configuration outside their allowed filesystem.
    fn global_ignore(&self) -> Option<ignore::gitignore::Gitignore> {
        None
    }
}

pub struct LocalFileSystem;
impl FileSystem for LocalFileSystem {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }
    fn write(&self, path: &Path, content: &str) -> io::Result<()> {
        std::fs::write(path, content)
    }
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        std::fs::create_dir_all(path)
    }
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
    fn read(&self, path: &Path, limit: u64) -> io::Result<Vec<u8>> {
        let file = std::fs::File::open(path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected a regular file",
            ));
        }
        let mut bytes = Vec::new();
        file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
        Ok(bytes)
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirectoryEntry>> {
        std::fs::read_dir(path)?
            .map(|entry| {
                let entry = entry?;
                let kind = entry.file_type()?;
                Ok(DirectoryEntry {
                    name: entry.file_name(),
                    kind: if kind.is_symlink() {
                        FileKind::Symlink
                    } else if kind.is_dir() {
                        FileKind::Directory
                    } else if kind.is_file() {
                        FileKind::File
                    } else {
                        FileKind::Other
                    },
                })
            })
            .collect()
    }
    fn global_ignore(&self) -> Option<ignore::gitignore::Gitignore> {
        Some(
            ignore::gitignore::GitignoreBuilder::new("")
                .build_global()
                .0,
        )
    }
}
