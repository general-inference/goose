# goose-context-management

Conversation compaction: summarizing a message history down to a single message so
a conversation can continue past a model's context window.

The crate is layered, smallest first — take the layer you need.

## `summarize`

Given a model and a slice of messages, produce one summary message.

```rust
use goose_context_management::{summarize, Templates};

let summary = summarize(&model, None, &Templates::default(), &messages).await?;
// summary.message, summary.usage
```

## `compact`

The trait-based API, for callers that own their own conversation representation.
Implement `CompactionInput` to expose messages (and optionally `Templates`), and
`CompactionOutput` to receive the summary and usage:

```rust
pub trait CompactionInput {
    fn messages(&self) -> Vec<Message>;
    fn templates(&self) -> Templates { Templates::default() }
}

pub trait CompactionOutput {
    fn set_summary(&mut self, summary: Message);
    fn set_usage(&mut self, usage: ProviderUsage);
}
```

`Vec<Message>` already implements `CompactionInput`, so the simple case needs no
wrapper type.

## Other exports

- `CompactionModel` — the model abstraction compaction runs against, with
  `ProviderModel` adapting any [`goose-providers`](../goose-providers) provider.
- `TokenEstimator` — optional token counting, used to decide how much history to
  feed the summarizer.
- `CompactingProvider` — a `Provider` wrapper that compacts as it goes.
- `StructuredSummary` / `FileActivity` — structured summary output, including
  which files the conversation touched.
- `Templates` and `format_message_for_compacting` — prompt shaping.
- `DEFAULT_COMPACTION_THRESHOLD` (`0.8`) — the default fraction of the context
  window at which callers compact.

## Cross-language access

Python and Kotlin reach compaction through [`goose-sdk`](../goose-sdk), which
wraps this crate in its UniFFI bindings. The trait-based `compact` API is Rust
only.

## Full conversation compaction (Rust)

`conversation::compact_messages` applies Goose's complete history transformation:
it preserves the original transcript, hides archived content from the model,
inserts the summary and continuation, and retains the current user request and
turn context for automatic compaction. It accepts `CompactionModel`, an optional
`TokenEstimator`, templates, a `Conversation`, and the manual/automatic flag.
The result contains the replacement conversation, billable summarization usage,
and an estimate of retained context. Persist the entire result before continuing.

The optional `token-counter` feature exposes Goose's cached `TokenCounter` and its
`TokenEstimator` implementation. `count_request_tokens` includes system text,
messages, and tool definitions. Keep one counter alive across requests to reuse
its cache. Neither the transformation nor a custom estimator requires the
built-in tokenizer feature.

Automatic triggering and bounded overflow recovery are available through
`goose-agent`'s native `context-management` feature. These APIs are Rust APIs;
this extraction does not add new Python or Kotlin bindings.

Hosts that need exact recent context can call
`conversation::compact_messages_with_recent_history` with a `RecentHistory`
turn count and token budget. Whole user turns, including tool exchanges, are
copied after the summary with agent-only visibility. The original transcript
remains unchanged for users. A token estimator is required for bounded
retention; disabled or oversized suffixes use the ordinary compaction policy.
Recent messages also inform the summary so it remains coherent. Automatic
compaction still preserves the active request when no bounded suffix fits.
