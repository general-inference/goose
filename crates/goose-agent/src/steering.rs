//! Queued guidance injected only after outstanding model and tool work settles.

use std::{
    collections::{HashSet, VecDeque},
    sync::{Arc, Mutex},
};

use anyhow::Result;
use async_trait::async_trait;
use goose_provider_types::{
    conversation::{message::Message, Conversation, EffectiveRole},
    maybe_send::{MaybeSend, MaybeSync},
};

use crate::operation::{
    applied, ends_turn, last_effective_role, messages_since_kickoff, not_applicable, Emitter,
    Operation, OperationResult,
};

/// Cloneable input handle. Keep it across runs to retain undelivered direction.
#[derive(Clone, Default)]
pub struct SteerQueue(Arc<Mutex<VecDeque<Message>>>);

impl SteerQueue {
    /// Assigns a stable identity before the message enters the running conversation.
    pub fn submit(&self, message: Message) -> Message {
        let message = message.with_steer().with_generated_id_if_missing();
        self.0.lock().unwrap().push_back(message.clone());
        message
    }

    /// Whether guidance remains queued for a future safe boundary.
    pub fn is_empty(&self) -> bool {
        self.0.lock().unwrap().is_empty()
    }

    /// Discards guidance that has not entered the conversation.
    pub fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
/// Optional host callback after guidance is emitted and before its effects return.
///
/// Emission is not persistence: confirm delivery only after the effect is committed.
pub trait SteerHook<S>: MaybeSend + MaybeSync {
    /// Observes a submitted message, with the same identity returned by the queue.
    async fn submitted(&self, session: &S, message: &Message);
}

/// Inserts queued messages in FIFO order after inference and outstanding tools settle.
///
/// Register before inference and end-of-turn operations. Use one instance per active
/// execution so cancellation can retain messages while a hook is awaiting.
pub struct SteerOperation<S> {
    queue: SteerQueue,
    hook: Option<Arc<dyn SteerHook<S>>>,
    kickoff_steering: bool,
    drained: std::sync::Mutex<Vec<Message>>,
}

impl<S> SteerOperation<S> {
    /// Creates an operation using a queue that can outlive the current execution.
    pub fn new(queue: SteerQueue) -> Self {
        Self {
            queue,
            hook: None,
            kickoff_steering: false,
            drained: std::sync::Mutex::default(),
        }
    }

    /// Also accepts guidance before inference when the conversation ends on a user message.
    ///
    /// Disabled by default. Enable for startup or interrupt/resume flows that must
    /// insert pending direction before the first model response. Outstanding tool
    /// requests still prevent insertion.
    pub fn with_kickoff_steering(mut self, enabled: bool) -> Self {
        self.kickoff_steering = enabled;
        self
    }

    /// Adds a host callback for each emitted message.
    pub fn with_hook(mut self, hook: Arc<dyn SteerHook<S>>) -> Self {
        self.hook = Some(hook);
        self
    }

    fn take_drained<E: From<Message>>(&self) -> Vec<E> {
        std::mem::take(&mut *self.drained.lock().unwrap())
            .into_iter()
            .map(E::from)
            .collect()
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl<S, E> Operation<S, E> for SteerOperation<S>
where
    S: MaybeSend + MaybeSync,
    E: From<Message> + MaybeSend + 'static,
{
    fn name(&self) -> &'static str {
        "steer"
    }

    async fn finalize_cancellation(
        &self,
        _session: &S,
        _conversation: &Conversation,
        _emit: &Emitter,
    ) -> Vec<E> {
        self.take_drained()
    }

    async fn run(
        &self,
        session: &S,
        conversation: &Conversation,
        emit: &Emitter,
    ) -> Result<OperationResult<E>> {
        let messages = messages_since_kickoff(conversation)?;
        let role = last_effective_role(messages)?;
        if !(ends_turn(messages)
            || role == EffectiveRole::Tool
            || (self.kickoff_steering && role == EffectiveRole::User))
        {
            return not_applicable();
        }
        let tool_messages = if role == EffectiveRole::User {
            conversation.messages()
        } else {
            messages
        };
        let answered: HashSet<_> = tool_messages
            .iter()
            .flat_map(Message::get_tool_response_ids)
            .collect();
        if tool_messages
            .iter()
            .flat_map(Message::get_tool_request_ids)
            .any(|id| !answered.contains(&id))
        {
            return not_applicable();
        }
        let pending: Vec<_> = self.queue.0.lock().unwrap().drain(..).collect();
        if pending.is_empty() {
            return not_applicable();
        }
        let drained: Vec<_> = pending
            .into_iter()
            .map(|message| emit.message(message))
            .collect();
        *self.drained.lock().unwrap() = drained.clone();
        if let Some(hook) = &self.hook {
            for message in drained {
                hook.submitted(session, &message).await;
            }
        }
        applied(self.take_drained())
    }
}
