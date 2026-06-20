use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use codex_code_mode_protocol::CreateCellRequest;
use codex_code_mode_protocol::FunctionCallOutputContentItem;
use pretty_assertions::assert_eq;
use serde_json::Value as JsonValue;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::session_runtime::OutputItem;

struct TestHost;

#[derive(Default)]
struct RecordingHost {
    committed: AtomicBool,
    notified: AtomicBool,
}

impl CellHost for TestHost {
    async fn invoke_tool(
        &self,
        _invocation: CellToolCall,
        _cancellation_token: CancellationToken,
    ) -> Result<JsonValue, String> {
        Err("unexpected tool call".to_string())
    }

    async fn notify(
        &self,
        _call_id: String,
        _text: String,
        _cancellation_token: CancellationToken,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn commit_completion(
        &self,
        _stored_value_writes: HashMap<String, JsonValue>,
        event: CellEvent,
        pending_yield_items: Option<Vec<OutputItem>>,
        cell_state: Arc<CellState>,
    ) -> bool {
        cell_state.commit_completion(event, pending_yield_items, || {})
    }

    async fn closed(&self) {}
}

impl CellHost for RecordingHost {
    async fn invoke_tool(
        &self,
        _invocation: CellToolCall,
        _cancellation_token: CancellationToken,
    ) -> Result<JsonValue, String> {
        Err("unexpected tool call".to_string())
    }

    async fn notify(
        &self,
        _call_id: String,
        _text: String,
        _cancellation_token: CancellationToken,
    ) -> Result<(), String> {
        self.notified.store(true, Ordering::Release);
        Ok(())
    }

    async fn commit_completion(
        &self,
        _stored_value_writes: HashMap<String, JsonValue>,
        event: CellEvent,
        pending_yield_items: Option<Vec<OutputItem>>,
        cell_state: Arc<CellState>,
    ) -> bool {
        cell_state.commit_completion(event, pending_yield_items, || {
            self.committed.store(true, Ordering::Release);
        })
    }

    async fn closed(&self) {}
}

struct CellActorHarness {
    event_tx: mpsc::UnboundedSender<RuntimeEvent>,
    handle: CellHandle,
    task: tokio::task::JoinHandle<()>,
    runtime_control_rx: std_mpsc::Receiver<RuntimeControlCommand>,
    _runtime_event_rx: mpsc::UnboundedReceiver<RuntimeEvent>,
}

fn spawn_cell_actor_harness() -> CellActorHarness {
    spawn_cell_actor_harness_with_host(Arc::new(TestHost))
}

fn spawn_cell_actor_harness_with_host<H: CellHost>(host: Arc<H>) -> CellActorHarness {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (runtime_event_tx, runtime_event_rx) = mpsc::unbounded_channel();
    let (runtime_tx, _runtime_control_tx, runtime_terminate_handle) = spawn_runtime(
        HashMap::new(),
        CreateCellRequest {
            tool_call_id: "call-1".to_string(),
            enabled_tools: Vec::new(),
            source: "await new Promise(() => {});".to_string(),
        },
        runtime_event_tx,
        PendingRuntimeMode::PauseUntilResumed,
    )
    .unwrap();
    let (runtime_control_tx, runtime_control_rx) = std_mpsc::channel();
    let cell_state = Arc::new(CellState::new(CancellationToken::new()));
    let handle = CellHandle::new(command_tx, Arc::clone(&cell_state));
    let task = tokio::spawn(run_cell(
        host,
        CellContext {
            runtime_tx,
            runtime_control_tx,
            runtime_terminate_handle,
            cell_state,
        },
        event_rx,
        command_rx,
    ));

    CellActorHarness {
        event_tx,
        handle,
        task,
        runtime_control_rx,
        _runtime_event_rx: runtime_event_rx,
    }
}

#[tokio::test]
async fn completion_and_output_are_buffered_until_the_first_observation() {
    let host = Arc::new(RecordingHost::default());
    let harness = spawn_cell_actor_harness_with_host(Arc::clone(&host));
    harness
        .event_tx
        .send(RuntimeEvent::ContentItem(
            FunctionCallOutputContentItem::InputText {
                text: "before observation".to_string(),
            },
        ))
        .unwrap();
    harness
        .event_tx
        .send(RuntimeEvent::Result {
            stored_value_writes: HashMap::new(),
            error_text: None,
        })
        .unwrap();
    while !host.committed.load(Ordering::Acquire) {
        tokio::task::yield_now().await;
    }

    assert_eq!(
        harness
            .handle
            .observe(ObserveMode::YieldAfter(Duration::ZERO))
            .await,
        Ok(CellEvent::Completed {
            content_items: vec![OutputItem::Text {
                text: "before observation".to_string(),
            }],
            error_text: None,
        })
    );
    harness.task.await.unwrap();
}

#[tokio::test]
async fn pending_frontier_waits_for_the_first_observation() {
    let host = Arc::new(RecordingHost::default());
    let harness = spawn_cell_actor_harness_with_host(Arc::clone(&host));
    harness.event_tx.send(RuntimeEvent::Pending).unwrap();
    harness
        .event_tx
        .send(RuntimeEvent::Notify {
            call_id: "notify-1".to_string(),
            text: "pending processed".to_string(),
        })
        .unwrap();
    while !host.notified.load(Ordering::Acquire) {
        tokio::task::yield_now().await;
    }

    assert!(matches!(
        harness.runtime_control_rx.try_recv(),
        Err(std_mpsc::TryRecvError::Empty)
    ));
    let observation = harness.handle.observe(ObserveMode::PendingFrontier);
    loop {
        match harness.runtime_control_rx.try_recv() {
            Ok(RuntimeControlCommand::Resume) => break,
            Ok(command) => panic!("expected resume, got {command:?}"),
            Err(std_mpsc::TryRecvError::Empty) => tokio::task::yield_now().await,
            Err(std_mpsc::TryRecvError::Disconnected) => {
                panic!("runtime control channel disconnected")
            }
        }
    }
    harness.event_tx.send(RuntimeEvent::Pending).unwrap();

    assert_eq!(
        observation.await,
        Ok(CellEvent::Pending {
            content_items: Vec::new(),
            pending_tool_call_ids: Vec::new(),
        })
    );

    let termination = harness.handle.terminate();
    drop(harness.event_tx);
    assert_eq!(
        termination.await,
        Ok(CellEvent::Terminated {
            content_items: Vec::new(),
        })
    );
    harness.task.await.unwrap();
}

#[tokio::test]
async fn buffered_yield_observation_resumes_an_unobserved_pending_frontier() {
    let host = Arc::new(RecordingHost::default());
    let harness = spawn_cell_actor_harness_with_host(Arc::clone(&host));
    harness.event_tx.send(RuntimeEvent::YieldRequested).unwrap();
    harness.event_tx.send(RuntimeEvent::Pending).unwrap();
    harness
        .event_tx
        .send(RuntimeEvent::Notify {
            call_id: "notify-1".to_string(),
            text: "pending processed".to_string(),
        })
        .unwrap();
    while !host.notified.load(Ordering::Acquire) {
        tokio::task::yield_now().await;
    }

    assert_eq!(
        harness
            .handle
            .observe(ObserveMode::YieldAfter(Duration::from_secs(60)))
            .await,
        Ok(CellEvent::Yielded {
            content_items: Vec::new(),
        })
    );
    loop {
        match harness.runtime_control_rx.try_recv() {
            Ok(RuntimeControlCommand::Continue) => break,
            Ok(command) => panic!("expected continue, got {command:?}"),
            Err(std_mpsc::TryRecvError::Empty) => tokio::task::yield_now().await,
            Err(std_mpsc::TryRecvError::Disconnected) => {
                panic!("runtime control channel disconnected")
            }
        }
    }

    host.notified.store(false, Ordering::Release);
    harness.event_tx.send(RuntimeEvent::Pending).unwrap();
    harness
        .event_tx
        .send(RuntimeEvent::Notify {
            call_id: "notify-2".to_string(),
            text: "later pending processed".to_string(),
        })
        .unwrap();
    while !host.notified.load(Ordering::Acquire) {
        tokio::task::yield_now().await;
    }
    assert!(matches!(
        harness.runtime_control_rx.try_recv(),
        Ok(RuntimeControlCommand::Continue)
    ));

    let termination = harness.handle.terminate();
    drop(harness.event_tx);
    assert_eq!(
        termination.await,
        Ok(CellEvent::Terminated {
            content_items: Vec::new(),
        })
    );
    harness.task.await.unwrap();
}

#[tokio::test]
async fn yield_timer_preempts_buffered_runtime_output() {
    let harness = spawn_cell_actor_harness();
    let initial_event = harness
        .handle
        .observe(ObserveMode::YieldAfter(Duration::ZERO));
    harness.event_tx.send(RuntimeEvent::Started).unwrap();
    harness
        .event_tx
        .send(RuntimeEvent::ContentItem(
            FunctionCallOutputContentItem::InputText {
                text: "queued output".to_string(),
            },
        ))
        .unwrap();

    assert_eq!(
        initial_event.await,
        Ok(CellEvent::Yielded {
            content_items: Vec::new(),
        })
    );

    let termination = harness.handle.terminate();
    drop(harness.event_tx);
    assert_eq!(
        termination.await,
        Ok(CellEvent::Terminated {
            content_items: vec![OutputItem::Text {
                text: "queued output".to_string(),
            }],
        })
    );
    harness.task.await.unwrap();
}

#[tokio::test]
async fn queued_termination_preempts_unobserved_runtime_completion() {
    let harness = spawn_cell_actor_harness();
    harness
        .event_tx
        .send(RuntimeEvent::Result {
            stored_value_writes: HashMap::new(),
            error_text: None,
        })
        .unwrap();
    let termination = harness.handle.terminate();

    let terminated = Ok(CellEvent::Terminated {
        content_items: Vec::new(),
    });
    assert_eq!(termination.await, terminated);
    harness.task.await.unwrap();
}

#[tokio::test]
async fn only_the_first_termination_claims_a_buffered_completion() {
    let cell_state = CellState::new(CancellationToken::new());
    let completion = CellEvent::Completed {
        content_items: Vec::new(),
        error_text: None,
    };
    assert!(cell_state.commit_completion(
        completion.clone(),
        /*pending_yield_items*/ None,
        || {}
    ));
    assert!(matches!(
        cell_state.deliver_completion(/*response_tx*/ None),
        CompletionDelivery::Buffered
    ));

    let first_termination = cell_state.request_termination();
    assert_eq!(
        cell_state.request_termination().await,
        Err(CellError::AlreadyTerminating)
    );
    assert_eq!(first_termination.await, Ok(completion.clone()));
    assert_eq!(
        cell_state.finish_termination(CellEvent::Terminated {
            content_items: Vec::new(),
        }),
        Some(completion)
    );
}

#[test]
fn failed_completion_delivery_rebuffers_the_event() {
    let cell_state = CellState::new(CancellationToken::new());
    let event = CellEvent::Completed {
        content_items: Vec::new(),
        error_text: None,
    };
    assert!(cell_state.commit_completion(event.clone(), /*pending_yield_items*/ None, || {}));
    let (response_tx, response_rx) = oneshot::channel();
    drop(response_rx);
    assert!(matches!(
        cell_state.deliver_completion(Some(response_tx)),
        CompletionDelivery::Buffered
    ));
    assert!(cell_state.accepting_observations());

    let (response_tx, mut response_rx) = oneshot::channel();
    assert!(matches!(
        cell_state.route_observation(ObserveMode::YieldAfter(Duration::ZERO), response_tx),
        ObservationDelivery::Delivered
    ));
    assert_eq!(response_rx.try_recv(), Ok(Ok(event)));
}

#[test]
fn buffered_yield_precedes_buffered_completion_for_yield_observer() {
    let cell_state = CellState::new(CancellationToken::new());
    let completion = CellEvent::Completed {
        content_items: vec![OutputItem::Text {
            text: "after".to_string(),
        }],
        error_text: None,
    };
    assert!(cell_state.commit_completion(
        completion.clone(),
        Some(vec![OutputItem::Text {
            text: "before".to_string(),
        }]),
        || {}
    ));
    assert!(matches!(
        cell_state.deliver_completion(/*response_tx*/ None),
        CompletionDelivery::Buffered
    ));

    let (response_tx, mut response_rx) = oneshot::channel();
    assert!(matches!(
        cell_state.route_observation(ObserveMode::YieldAfter(Duration::ZERO), response_tx),
        ObservationDelivery::Buffered
    ));
    assert_eq!(
        response_rx.try_recv(),
        Ok(Ok(CellEvent::Yielded {
            content_items: vec![OutputItem::Text {
                text: "before".to_string(),
            }],
        }))
    );

    let (response_tx, mut response_rx) = oneshot::channel();
    assert!(matches!(
        cell_state.route_observation(ObserveMode::YieldAfter(Duration::ZERO), response_tx),
        ObservationDelivery::Delivered
    ));
    assert_eq!(response_rx.try_recv(), Ok(Ok(completion)));
}

#[test]
fn pending_observer_merges_buffered_yield_and_completion_output() {
    let cell_state = CellState::new(CancellationToken::new());
    assert!(cell_state.commit_completion(
        CellEvent::Completed {
            content_items: vec![OutputItem::Text {
                text: "after".to_string(),
            }],
            error_text: None,
        },
        Some(vec![OutputItem::Text {
            text: "before".to_string(),
        }]),
        || {}
    ));
    assert!(matches!(
        cell_state.deliver_completion(/*response_tx*/ None),
        CompletionDelivery::Buffered
    ));

    let (response_tx, mut response_rx) = oneshot::channel();
    assert!(matches!(
        cell_state.route_observation(ObserveMode::PendingFrontier, response_tx),
        ObservationDelivery::Delivered
    ));
    assert_eq!(
        response_rx.try_recv(),
        Ok(Ok(CellEvent::Completed {
            content_items: vec![
                OutputItem::Text {
                    text: "before".to_string(),
                },
                OutputItem::Text {
                    text: "after".to_string(),
                },
            ],
            error_text: None,
        }))
    );
}
