# goose-developer

Native Goose developer tools for Rust GDK applications: `write`, `edit`, `shell`,
`tree`, and `read_image`. Goose's developer extension uses this same implementation.
Tool schemas, instructions in `prompts/`, and behavior tests are owned here.

Register `Developer` with `goose_agent::tool::ToolOperation::with_provider`.
The host session implements `DeveloperSession` to supply the working directory,
session identifier, and default timeout. The GDK emitter carries shell output
notifications and cancellation. Keep the provider alive so saved shell output
remains available between calls.

These are native tools, not a sandbox. The embedding application owns approvals,
filesystem permissions, network policy, credentials, and process isolation.
No session storage or separate helper executable is required.

From the repository root: `cargo test -p goose-developer` and
`cargo clippy -p goose-developer --all-targets -- -D warnings`.
