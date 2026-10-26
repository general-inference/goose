//! Filesystem operations used by the shared text-file tools.
use std::{io, path::Path};

/// Hosts enforce access during each operation, including symlink resolution.
/// A path check followed by unrestricted I/O does not provide confinement.
pub trait FileSystem: Send + Sync {
    fn read_to_string(&self, path: &Path) -> io::Result<String>;
    fn write(&self, path: &Path, content: &str) -> io::Result<()>;
    fn create_dir_all(&self, path: &Path) -> io::Result<()>;
    fn exists(&self, path: &Path) -> bool;
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
}
