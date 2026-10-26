//! Repository instructions and @file imports shared with Goose.
//!
//! Hosts choose the instruction filenames and global paths. Every filesystem
//! operation uses the supplied backend; path validation does not replace the
//! backend's responsibility to enforce access at the actual operation.
mod filesystem;
mod import_files;
mod load_hints;
pub use filesystem::{HintFileSystem, HintMetadata, NativeHintFileSystem};
pub use load_hints::{
    build_gitignore, load_hint_files, SubdirectoryHintTracker, AGENTS_MD_FILENAME,
    GOOSE_HINTS_FILENAME,
};
