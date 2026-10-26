#[cfg(all(feature = "context-management", not(target_arch = "wasm32")))]
pub mod compaction;
pub mod events;
pub mod goal;
pub mod inference;
pub mod machine;
pub mod operation;
pub mod skills;
pub mod subagent;
pub mod tool;

pub mod permission;

#[cfg(all(not(target_arch = "wasm32"), feature = "project-context"))]
pub mod project_context;

#[cfg(feature = "prompt")]
pub mod prompt;

pub mod turn_context;
