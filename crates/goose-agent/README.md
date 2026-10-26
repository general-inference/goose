# goose-agent

The GDK's agent loop, unrolled into a state machine you assemble yourself.

Instead of a fixed "call the model, run the tools, repeat" loop, an agent here is
an ordered list of steps. Each step gets a chance to look at the conversation and
either decline or produce effects. The machine walks the list, applies the first
step that applies, and starts over from the top — so the whole agent's behavior
is a function of the persisted conversation, not of in-memory loop state.

## The pieces

- **`Operation<S, E>`** — one step. Implement `run` to act on the conversation,
  plus any of `inference_tools`, `prompt_parts`, and `moim_parts` to contribute
  to the model request. Returns `OperationResult::NotApplicable` to pass, or
  `applied(..)` / `yielded(..)` to take the step. Helpers: `not_applicable()`,
  `applied()`, `yielded()`, `yielded_with()`.
- **`Inference<S, E>`** — the step that reaches the provider. Before calling it,
  the machine collects tools and prompt parts from *every* operation in the list
  into an `InferenceInput`. It runs whenever the conversation ends on a user or
  tool message, even if other tool calls from the same reply are still
  unanswered. A pipeline that keeps calls open, for example while waiting for
  the user to approve one, needs an operation before inference that yields
  while any call is open.
- **`ToolOperation<S>`** — adapts caller-defined `rmcp` tools into one operation.
  Register `SyncTool<S>` and `AsyncTool<S>` implementations with the typed builder
  methods, or implement `ToolProvider<S>` for tools discovered dynamically per
  session. Dynamic providers are queried before inference and again before
  execution, so their tool names and handlers must remain stable between those
  boundaries. Their schemas are sent to inference and matching calls are
  dispatched against the current session.
- **`StateMachine<'a, S, E>`** — holds `Vec<Step<..>>` and a `CancellationToken`.
  `step()` runs one pass and returns a `StepResult` whose `applied_step` names
  the step that applied (`None` if none did) and whose `status` says whether the
  run is `Continuing`, `Yielded` to the client, or `Cancelled`; `apply()` writes
  effects back; `run()` loops while `status` is `Continuing`.
- **`ConversationEffect`** — the default effect type: `AppendMessage`,
  `ReplaceConversation`, `PatchToolRequestMeta`, `SetMessageVisibility`. Bring
  your own by implementing `MachineEffect`.
- **`Emitter`** — streams `AgentEvent`s (`Message`, `Usage`, `MessageUsage`,
  `McpNotification`, `HistoryReplaced`) to the client while a step runs, and
  carries the cancellation token.
- **`SessionLoader`** / **`EffectHandler`** / **`MachineSession`** — the traits
  your runtime implements so the machine can load a session by id and persist
  effects. The machine reloads the session between passes; it never caches it.

## Reading the conversation

Because steps re-derive their decisions from history, the crate ships the
predicates they need: `messages_since_kickoff`, `last_effective_role`,
`assistant_turn_count`, `ends_turn`, and `trailing_error`. When a step must
remember that it already did something, it records that on the message itself via
`Operation::set_message_meta` / `message_meta` rather than in memory.

## Cancellation

On Stop, the machine drops the running step's future, including inference
preparation. It then saves the interrupted step's `finalize_cancellation`
effects, answers every tool request since kickoff that still has no response
with "Tool call was interrupted before completing", and finally calls
`finalize_cancellation` on the remaining operations in pipeline order. Every
operation's `finalize_cancellation` runs once, and the session is reloaded
after each save. Callers driving `step` and `apply`
themselves must call `finalize` on exit.

An operation implements `finalize_cancellation` only when it holds received
output across a later await, or owns work that must finish. Keep that output in
the operation, not in locals of the dropped future, and drain the same field on
normal completion. `finalize_cancellation` emits only what the client has not
seen yet.
`InferenceRunner` holds its streamed messages and usage; `ToolOperation` holds
its tool response message. Use one operation instance per active execution.

`Emitter` sends on an unbounded channel, so emitting never pauses and an
operation that only builds, emits, and returns holds nothing. Dropping a future
cannot undo external actions or stop blocking work that has already started.

## Skills

`skills` provides argument substitution and loaded-skill formatting. On native
platforms, it also discovers skills from explicit, ordered roots. Configuration
paths, built-in skills, and tool registration remain host responsibilities;
these helpers do not automatically change the agent pipeline. Filesystem
discovery is not a secure file-open API.

## WebAssembly

The crate builds for `wasm32-unknown-unknown` and runs without a tokio runtime,
so it can be embedded in a JavaScript host. There, the clock and randomness come
from JavaScript. Some things differ from native:

- The traits you implement (`Provider`, `Operation`, `ToolProvider`,
  `SessionLoader`, `EffectHandler`, and the rest) do not require `Send` or
  `Sync`, and neither does `MessageStream`. So you can await JavaScript promises
  and hold host handles directly. Implement the traits with
  `#[async_trait(?Send)]`. The bounds are `MaybeSend` and `MaybeSync` from
  `goose_provider_types::maybe_send`, which are the real `Send` and `Sync`
  natively.
- `SyncTool`s run inline instead of on a blocking thread, so cancellation cannot
  interrupt one that has started.
- `ProviderRetry` waits between attempts on the host's `setTimeout` instead of
  tokio's timer, so retries work without a tokio runtime.

The reference assembly of these pieces is `goose::agents::state_machine` in the
[`goose`](../goose) crate.

## Context management (native Rust)

Enable `context-management` to use `compaction::CompactionOperation`.
Supply `ModelCompactor` with a `CompactionModel`, a shared `TokenEstimator`, and
prompt templates, or implement `SessionCompactor` for session-specific model
selection. `CompactionSession` supplies optional previously reported usage;
`CompactionEffect` lets the host persist the replacement conversation and
summarization usage together. Register the operation only when the provider
does not manage its own context.

Use the same operation with `InferenceRunner::with_request_guard`. The operation
handles manual compaction and bounded context-overflow recovery; the request guard
also accounts for the fully prepared system prompt, message projection, and tool
schemas before sending inference. A guard that applies returns effects for the
state machine to persist before it prepares another request.

Compaction marks generated messages in their persisted operation metadata.
`assistant_turn_count` excludes those messages, and proactive compaction will not
repeat on an unchanged compacted tail. An irreducibly large request is attempted
once and then follows bounded overflow recovery. Token counts remain estimates;
this cannot guarantee that a provider accepts a request.

## Goal review

`goal::GoalReviewOperation` exposes the one-time goal check used by Goose's
retry operation. Register it before inference with the user request as its goal
to give a completed response one opportunity to verify the work and continue.
The invisible review message persists in conversation metadata so restoring a
conversation does not repeat the review in the same turn. Explicit output limits,
model errors and pending tool requests are not converted into reviews.

This is model self-review, not independent proof of task success. Keep the host's
turn limits, approvals, cancellation and external verification in place.

`operation::trailing_assistant_text` selects the user-visible final assistant
block for an answer or stop-hook context. It excludes earlier tool narration and
hidden content; Goose's stop hooks use this same implementation.

## Turn context

`turn_context` exposes the same operational-context message and turn-budget rendering
used by Goose. Hosts supply the workspace, model context limit, stable formatted
timestamp and contextual parts; `turn_context_event` returns an agent-only message
marked for persistence. `system_prompt_block` explains its meaning to the model.
`update` suppresses unchanged blocks and refreshes context after goal review.
Configuration, clock reads and extension leasing stay with the host. The shared implementation preserves Goose's small-context threshold
and escapes workspace/time fields while retaining contributed text literally.

## Prompt composition

Enable `prompt` for the system-prompt builder and template renderer used by Goose.
`prompt::SystemPromptBuilder` accepts explicit template text, time, permission mode,
extension instructions and ordered operation contributions. Hosts load configuration,
custom templates and repository hints through their own policies; the builder does
not read files or environment variables. `SYSTEM_TEMPLATE` is Goose's built-in
Markdown template; another host can supply its own identity template.

Extension instructions, the base template and appended contributions receive Goose's
Unicode-tag sanitization. Contributions are appended as literal data, not parsed as
another template. Duplicate contribution keys replace values without moving their
position; supplied root hints replace the `hints` entry. Chat mode adds the native
no-tools instruction. A host with its own tool catalog should use
`without_extensions()` and pass its operation contributions explicitly.

`prompt::render` preserves Goose's whitespace rules and `code_fence` filter for other
Markdown templates. It renders only supplied data and does not load template paths.

## Repository instructions

The native `project-context` feature exposes Goose's hint loader through
`project_context`: configured instruction filenames, explicit global hint paths,
Git-root ancestor loading, `.gitignore` filtering, bounded `@file` expansion and
`SubdirectoryHintTracker`. Goose uses the same implementation.

Hosts supply `HintFileSystem` for every read, metadata query and canonicalization.
`NativeHintFileSystem` provides ordinary native access; it is not a sandbox.
Restricted hosts must enforce access during actual operations, including symlink
races. They may reject additional files or impose smaller read limits. Passing no
global paths prevents global hints from being loaded. The tracker can be rebuilt
from persisted tool arguments before inference, without separately persisting its
in-memory state. It discovers hints after tool arguments are recorded; it does not
intercept or authorize a tool's first write.
