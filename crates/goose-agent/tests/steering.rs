use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use goose_agent::{
    events::AgentEvent,
    machine::{EffectHandler, MachineSession, SessionLoader, StateMachine, Step},
    operation::{
        yielded, ConversationEffect, Emitter, Inference, InferenceInput, Operation, OperationResult,
    },
    steering::{SteerHook, SteerOperation, SteerQueue},
};
use goose_provider_types::conversation::{message::Message, Conversation};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn conversation() -> Conversation {
    Conversation::new_unvalidated([
        Message::user().with_text("paint it"),
        Message::assistant().with_text("starting"),
    ])
}

fn appended(effects: Vec<ConversationEffect>) -> Vec<Message> {
    effects
        .into_iter()
        .map(|effect| match effect {
            ConversationEffect::AppendMessage(message) => message,
            _ => panic!("unexpected effect"),
        })
        .collect()
}

#[tokio::test]
async fn preserves_submitted_identity_and_fifo_through_emission_and_effects() {
    let queue = SteerQueue::default();
    let first = queue.submit(Message::user().with_text("use blue"));
    let second = queue
        .clone()
        .submit(Message::user().with_text("make it matte").with_id("known"));
    assert!(first.id.is_some());
    assert_ne!(first.id, second.id);
    assert_eq!(second.id.as_deref(), Some("known"));
    assert!(first.metadata.steer && second.metadata.steer);

    let operation = SteerOperation::new(queue.clone());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let emit = Emitter::new(tx, CancellationToken::new());
    let result: OperationResult = operation.run(&(), &conversation(), &emit).await.unwrap();
    let OperationResult::Applied(result) = result else {
        panic!("steering did not apply")
    };
    let messages = appended(result.effects);
    assert_eq!(messages, [first, second]);
    for message in messages {
        let AgentEvent::Message(emitted) = rx.try_recv().unwrap() else {
            panic!("unexpected event")
        };
        assert_eq!(emitted, message);
    }
    assert!(rx.try_recv().is_err());
    assert!(queue.is_empty());
    let repeated: OperationResult = operation.run(&(), &conversation(), &emit).await.unwrap();
    assert!(matches!(repeated, OperationResult::NotApplicable));
    queue.submit(Message::user().with_text("discard this"));
    queue.clear();
    assert!(queue.is_empty());
}

#[tokio::test]
async fn waits_until_every_tool_request_is_answered() {
    let queue = SteerQueue::default();
    let submitted = queue.submit(Message::user().with_text("use blue"));
    let operation = SteerOperation::new(queue.clone());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let emit = Emitter::new(tx, CancellationToken::new());
    let mut conversation = Conversation::new_unvalidated([
        Message::user().with_text("paint it"),
        Message::assistant()
            .with_tool_request("first", Ok(CallToolRequestParams::new("paint")))
            .with_tool_request("second", Ok(CallToolRequestParams::new("dry"))),
    ]);
    let result: OperationResult = operation.run(&(), &conversation, &emit).await.unwrap();
    assert!(matches!(result, OperationResult::NotApplicable));
    conversation
        .push(Message::user().with_tool_response("first", Ok(CallToolResult::success(vec![]))));
    let partial: OperationResult = operation.run(&(), &conversation, &emit).await.unwrap();
    assert!(matches!(partial, OperationResult::NotApplicable));
    assert!(!queue.is_empty());
    assert!(rx.try_recv().is_err());

    conversation
        .push(Message::user().with_tool_response("second", Ok(CallToolResult::success(vec![]))));
    let settled: OperationResult = operation.run(&(), &conversation, &emit).await.unwrap();
    let OperationResult::Applied(result) = settled else {
        panic!("steering did not apply after tools settled")
    };
    assert_eq!(appended(result.effects), [submitted]);
    assert!(queue.is_empty());
}

#[tokio::test]
async fn kickoff_steering_does_not_hide_unresolved_tools_behind_a_user_tail() {
    let queue = SteerQueue::default();
    queue.submit(Message::user().with_text("use blue"));
    let operation = SteerOperation::new(queue.clone()).with_kickoff_steering(true);
    let conversation = Conversation::new_unvalidated([
        Message::user().with_text("paint it"),
        Message::assistant().with_tool_request("first", Ok(CallToolRequestParams::new("paint"))),
        Message::user().with_text("continue"),
    ]);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let emit = Emitter::new(tx, CancellationToken::new());
    let result: OperationResult = operation.run(&(), &conversation, &emit).await.unwrap();
    assert!(matches!(result, OperationResult::NotApplicable));
    assert!(!queue.is_empty());
    assert!(rx.try_recv().is_err());
}

#[derive(Clone)]
struct Session(Conversation);

impl MachineSession for Session {
    fn id(&self) -> &str {
        "session"
    }

    fn conversation(&self) -> Option<&Conversation> {
        Some(&self.0)
    }
}

struct Runtime(Mutex<Session>);

#[async_trait]
impl SessionLoader<Session> for Runtime {
    async fn load(&self, _session_id: &str) -> Result<Session> {
        Ok(self.0.lock().unwrap().clone())
    }
}

#[async_trait]
impl EffectHandler<Session, ConversationEffect> for Runtime {
    async fn apply_effects(
        &self,
        _session: &Session,
        effects: &mut [ConversationEffect],
        _emit: &Emitter,
    ) -> Result<()> {
        for effect in effects {
            let ConversationEffect::AppendMessage(message) = effect else {
                panic!("unexpected effect")
            };
            self.0.lock().unwrap().0.push(message.clone());
        }
        Ok(())
    }
}

struct PendingHook {
    cancel: CancellationToken,
    calls: Mutex<Vec<Message>>,
}

#[async_trait]
impl SteerHook<Session> for PendingHook {
    async fn submitted(&self, _session: &Session, message: &Message) {
        self.calls.lock().unwrap().push(message.clone());
        self.cancel.cancel();
        std::future::pending().await
    }
}

#[tokio::test]
async fn cancellation_during_hook_commits_every_drained_message_once() {
    let cancel = CancellationToken::new();
    let queue = SteerQueue::default();
    let first = queue.submit(Message::user().with_text("use blue"));
    let second = queue.submit(Message::user().with_text("make it matte"));
    let hook = Arc::new(PendingHook {
        cancel: cancel.clone(),
        calls: Mutex::new(vec![]),
    });
    let operation = Arc::new(SteerOperation::new(queue.clone()).with_hook(hook.clone()));
    let machine = StateMachine::new(vec![Step::Operation(operation)], cancel.clone());
    let runtime = Runtime(Mutex::new(Session(conversation())));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let emit = Emitter::new(tx, cancel);
    let session = machine.run(&runtime, "session", &emit).await.unwrap();
    assert_eq!(&session.0.messages()[2..], [first.clone(), second.clone()]);
    assert_eq!(
        hook.calls.lock().unwrap().as_slice(),
        std::slice::from_ref(&first)
    );
    assert!(queue.is_empty());
    for message in [first, second] {
        let AgentEvent::Message(emitted) = rx.try_recv().unwrap() else {
            panic!("unexpected event")
        };
        assert_eq!(emitted, message);
    }
    assert!(rx.try_recv().is_err());
    let finalized_again = machine.finalize(&runtime, "session", &emit).await.unwrap();
    assert_eq!(finalized_again.0, session.0);
    assert!(rx.try_recv().is_err());
}

struct RecordingInference {
    cancel: Option<CancellationToken>,
    calls: Mutex<Vec<Conversation>>,
}

#[async_trait]
impl Operation<Session> for RecordingInference {
    fn name(&self) -> &'static str {
        "recording inference"
    }
}

#[async_trait]
impl Inference<Session> for RecordingInference {
    fn applies(&self, _conversation: &Conversation) -> bool {
        true
    }

    async fn infer(
        &self,
        _session: &Session,
        conversation: &Conversation,
        _input: InferenceInput,
        _emit: &Emitter,
    ) -> Result<OperationResult> {
        self.calls.lock().unwrap().push(conversation.clone());
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
            return std::future::pending().await;
        }
        yielded()
    }
}

#[tokio::test]
async fn kickoff_steering_reaches_first_resumed_inference_after_cancel_without_response() {
    let cancel = CancellationToken::new();
    let queue = SteerQueue::default();
    let first = queue.submit(Message::user().with_text("use blue"));
    let second = queue.submit(Message::user().with_text("make it matte").with_id("known"));
    let kickoff = Conversation::new_unvalidated([Message::user().with_text("paint it")]);
    let runtime = Runtime(Mutex::new(Session(kickoff.clone())));
    let interrupted_inference = Arc::new(RecordingInference {
        cancel: Some(cancel.clone()),
        calls: Mutex::new(vec![]),
    });
    let interrupted = StateMachine::new(
        vec![
            Step::Operation(Arc::new(SteerOperation::new(queue.clone()))),
            Step::Inference(interrupted_inference.clone()),
        ],
        cancel.clone(),
    );
    let (tx, mut rx) = mpsc::unbounded_channel();
    let emit = Emitter::new(tx, cancel);
    let cancelled = interrupted.run(&runtime, "session", &emit).await.unwrap();
    assert_eq!(cancelled.0, kickoff);
    assert_eq!(*interrupted_inference.calls.lock().unwrap(), [kickoff]);
    assert!(!queue.is_empty());
    assert!(rx.try_recv().is_err());

    let cancel = CancellationToken::new();
    let resumed_inference = Arc::new(RecordingInference {
        cancel: None,
        calls: Mutex::new(vec![]),
    });
    let resumed = StateMachine::new(
        vec![
            Step::Operation(Arc::new(
                SteerOperation::new(queue.clone()).with_kickoff_steering(true),
            )),
            Step::Inference(resumed_inference.clone()),
        ],
        cancel.clone(),
    );
    let (tx, _rx) = mpsc::unbounded_channel();
    let session = resumed
        .run(&runtime, "session", &Emitter::new(tx, cancel))
        .await
        .unwrap();
    assert_eq!(&session.0.messages()[1..], [first, second]);
    assert_eq!(
        resumed_inference.calls.lock().unwrap().as_slice(),
        std::slice::from_ref(&session.0)
    );
    assert!(queue.is_empty());
}
