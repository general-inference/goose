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

For restricted hosts, supply `EditTools::with_filesystem` and
`ShellTool::with_executor`, then attach them with `Developer::with_edit_tools`
and `Developer::with_shell_tool`. The shared tool code retains exact-match edits,
streaming, timeouts, and output handling; the host supplies filesystem operations
and process creation. A `RunningShell` must stop active processes on drop.

Select the permitted tools with `Developer::with_tools`. This filters both
advertising and dispatch. For directory and image access, attach
`TreeTool::with_filesystem` and `ImageTool::with_backends` using `with_tree_tool`
and `with_image_tool`. All traversal, ignore-file reads, line counts, and image
bytes go through the host filesystem. `ImageHttp` must enforce host network policy
during connections and redirects. Image decoding and cropping remain shared Goose
code. Unimplemented filesystem operations fail closed; they never fall back to
native access. These interfaces do not implement a sandbox themselves.

`ShellExecutor` receives the command text and session context before any shell
process or Flatpak environment probing occurs. Hosts can supply their own process
handles and output streams. `ShellTool::with_output_directory` keeps saved output
inside a host-approved temporary directory when needed.
