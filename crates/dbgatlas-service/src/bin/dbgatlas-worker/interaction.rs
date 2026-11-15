use super::*;
use dbgatlas_dbgeng::{ActivityKind, InterruptHandle, InterruptKind};
use dbgatlas_worker_protocol::{
    WorkerControlAction, WorkerControlRequest, WorkerControlResponse, WorkerMessage, WorkerSnapshot,
};
use std::sync::{Arc, Mutex, mpsc};

#[derive(Clone)]
pub(super) struct Activity {
    epoch: u64,
    operation: Option<OperationRef>,
    kind: ActivityKind,
}

#[derive(Default)]
pub(super) struct Shared {
    pub control: Option<InterruptHandle>,
    pub artifact_dir: Option<PathBuf>,
    active: Option<Activity>,
    reserved: bool,
    closing: bool,
    stop: bool,
    snapshot: Option<WorkerSnapshot>,
    sequence: u64,
    transport_gaps: u64,
}

type Frame = WorkerEnvelope<WorkerMessage>;
type Outgoing = (Frame, Option<mpsc::SyncSender<()>>);

fn response(id: String, result: WorkerResponse, snapshot: Option<WorkerSnapshot>) -> Frame {
    let mut frame = WorkerEnvelope::new(id, WorkerMessage::Response { result });
    frame.snapshot = snapshot;
    frame
}

fn wake_wait(shared: &Arc<Mutex<Shared>>) {
    let candidate = {
        let state = shared.lock().unwrap();
        state.active.clone().zip(state.control.clone())
    };
    if let Some((activity, control)) = candidate {
        if activity.kind != ActivityKind::EventWait {
            return;
        }
        let started = Instant::now();
        loop {
            match control.interrupt(activity.epoch, InterruptKind::ExitWait) {
                Ok(hr)
                    if hr == 0x8000000au32 as i32 && started.elapsed() < Duration::from_secs(2) =>
                {
                    thread::sleep(Duration::from_millis(5))
                }
                _ => break,
            }
        }
    }
}

fn control_request(
    shared: &Arc<Mutex<Shared>>,
    expected_session: &str,
    request: WorkerControlRequest,
) -> WorkerControlResponse {
    let rejected = |code: &str| WorkerControlResponse {
        accepted: false,
        code: code.into(),
        epoch: None,
        native_hresult: None,
    };
    if request.session_id.id.as_str() != expected_session {
        return rejected("session_mismatch");
    }
    let (activity, control) = {
        let state = shared.lock().unwrap();
        if state.closing {
            return rejected("session_closing");
        }
        let Some(activity) = state.active.clone() else {
            return rejected("no_active_execution");
        };
        let Some(control) = state.control.clone() else {
            return rejected("unsupported");
        };
        (activity, control)
    };
    let kind = match request.action {
        WorkerControlAction::Cancel { operation_id }
            if activity.kind == ActivityKind::Command
                && activity.operation.as_ref() == Some(&operation_id) =>
        {
            InterruptKind::Passive
        }
        WorkerControlAction::Interrupt { expected_epoch }
            if activity.kind == ActivityKind::EventWait && activity.epoch == expected_epoch =>
        {
            InterruptKind::Active
        }
        _ => return rejected("stale_execution"),
    };
    match control.interrupt(activity.epoch, kind) {
        Ok(hr) => WorkerControlResponse {
            accepted: hr >= 0,
            code: if hr >= 0 {
                "interrupt_requested"
            } else {
                "native_interrupt_failed"
            }
            .into(),
            epoch: Some(activity.epoch),
            native_hresult: Some(hr),
        },
        Err(error) => WorkerControlResponse {
            accepted: false,
            code: error.to_string(),
            epoch: Some(activity.epoch),
            native_hresult: None,
        },
    }
}

fn stamp(state: &mut Shared) -> Option<WorkerSnapshot> {
    state.sequence += 1;
    let snapshot = state.snapshot.as_mut()?;
    snapshot.sequence = state.sequence;
    snapshot.observed_at = Timestamp::now();
    snapshot.context["engine"]["transport_gaps"] = serde_json::json!(state.transport_gaps);
    Some(snapshot.clone())
}

fn owner_snapshot(
    worker: &WorkerState,
    phase: &str,
    epoch: Option<u64>,
    native_hr: Option<i32>,
) -> Option<WorkerSnapshot> {
    let mut observed = worker.snapshot()?;
    let mut state = worker.interaction.lock().unwrap();
    if let Some(previous) = state.snapshot.as_ref() {
        if previous.target_state == dbgatlas_model::TargetState::Exited
            && observed.target_state == dbgatlas_model::TargetState::Unknown
        {
            observed.target_state = dbgatlas_model::TargetState::Exited;
        }
        for name in [
            "engine",
            "last_event",
            "input_required",
            "output",
            "execution_start",
        ] {
            if let Some(value) = previous.context.get(name) {
                observed.context[name] = value.clone();
            }
        }
    }
    observed.context["engine"]["state"] = serde_json::json!(phase);
    observed.context["engine"]["epoch"] = serde_json::json!(epoch);
    observed.context["engine"]["native_hresult"] = serde_json::json!(native_hr);
    observed.context["engine"]["active_operation_id"] =
        serde_json::json!(state.active.as_ref().and_then(|a| a.operation.clone()));
    if phase == "executing" {
        observed.context["execution_start"] = serde_json::json!({
            "operation_id":state.active.as_ref().and_then(|a|a.operation.clone()),
            "observed_at":observed.observed_at,"target_process":observed.target_process,
            "context":{"thread_id":observed.context["thread_id"],"frame_index":observed.context["frame_index"],"source":"engine_owner_at_start"}
        });
    }
    state.snapshot = Some(observed);
    stamp(&mut state)
}

fn start_events(
    shared: Arc<Mutex<Shared>>,
    output: mpsc::SyncSender<Outgoing>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut previous_output = (0, 0);
        loop {
            let (control, dir, stop) = {
                let state = shared.lock().unwrap();
                (
                    state.control.clone(),
                    state.artifact_dir.clone(),
                    state.stop,
                )
            };
            if let (Some(control), Some(dir)) = (control, dir) {
                if let Ok(batch) = control.read_events() {
                    let changed = !batch.events.is_empty()
                        || previous_output != (batch.output_byte_len, batch.output_error);
                    if changed {
                        previous_output = (batch.output_byte_len, batch.output_error);
                        let events: Vec<_> = batch
                            .events
                            .iter()
                            .map(|e| serde_json::to_value(e).unwrap())
                            .collect();
                        let persisted = append_event(
                            &dir.join("engine-events.jsonl"),
                            &serde_json::json!({"observed_at":Timestamp::now(),"events":events,"output_byte_len":batch.output_byte_len,"output_error":batch.output_error}),
                        );
                        let snapshot = {
                            let mut state = shared.lock().unwrap();
                            if let Some(snapshot) = state.snapshot.as_mut() {
                                for event in &batch.events {
                                    match event.kind {
                                        1 | 2 | 3 => {
                                            snapshot.context["last_event"] =
                                                serde_json::to_value(event).unwrap();
                                            if event.kind == 3
                                                && snapshot.target_state
                                                    != dbgatlas_model::TargetState::Offline
                                            {
                                                snapshot.target_state =
                                                    dbgatlas_model::TargetState::Exited;
                                            }
                                        }
                                        4 => {
                                            snapshot.context["engine"]["backend_status"] =
                                                serde_json::json!(event.argument)
                                        }
                                        5 => {
                                            snapshot.context["input_required"] =
                                                serde_json::json!(event.code != 0)
                                        }
                                        6 => {
                                            snapshot.context["engine"]["event_gap"] =
                                                serde_json::json!(event.argument)
                                        }
                                        _ => {}
                                    }
                                }
                                snapshot.context["output"] = serde_json::json!({"path":"session-output.log","byte_len":batch.output_byte_len,"error":batch.output_error,"event_log_error":persisted.err().map(|e|e.to_string())});
                            }
                            stamp(&mut state)
                        };
                        let mut frame =
                            WorkerEnvelope::new("event", WorkerMessage::Event { events });
                        frame.snapshot = snapshot;
                        if output.try_send((frame, None)).is_err() {
                            shared.lock().unwrap().transport_gaps += 1;
                        }
                    }
                }
            }
            if stop {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
    })
}

pub(super) fn run(
    args: WorkerArgs,
    mut worker: WorkerState,
    pipe: PipeStream,
    mut control_pipe: PipeStream,
) -> Result<(), Box<dyn std::error::Error>> {
    let shared = worker.interaction.clone();
    let (output, outgoing) = mpsc::sync_channel::<Outgoing>(64);
    let mut writer = pipe.try_clone()?;
    let mut pipe = std::io::BufReader::new(pipe);
    let writer_state = shared.clone();
    let writer_thread = thread::spawn(move || {
        for (frame, ack) in outgoing {
            let result = encode_jsonl(&frame)
                .map_err(std::io::Error::other)
                .and_then(|line| {
                    writer
                        .write_all(line.as_bytes())
                        .and_then(|_| writer.flush())
                });
            if result.is_err() {
                writer_state.lock().unwrap().stop = true;
                wake_wait(&writer_state);
                break;
            }
            if let Some(ack) = ack {
                let _ = ack.try_send(());
            }
        }
    });
    let (requests, incoming) = mpsc::sync_channel::<WorkerEnvelope<WorkerRequest>>(1);
    let reader_state = shared.clone();
    let reader_output = output.clone();
    let session_id = args.session_id.clone();
    thread::spawn(move || {
        while let Ok(line) = read_jsonl_line(&mut pipe) {
            let Ok(request) = decode_jsonl::<WorkerRequest>(&line) else {
                break;
            };
            let terminal = matches!(
                request.message,
                WorkerRequest::CloseSession { .. } | WorkerRequest::KillSession { .. }
            );
            let rejected = {
                let mut state = reader_state.lock().unwrap();
                if request.message.session_id().id.as_str() != session_id {
                    Some("session_mismatch")
                } else if state.closing {
                    Some("session_closing")
                } else if terminal {
                    state.closing = true;
                    None
                } else if state.reserved || state.active.is_some() {
                    Some("session_busy")
                } else {
                    state.reserved = true;
                    None
                }
            };
            if let Some(code) = rejected {
                let snapshot = reader_state.lock().unwrap().snapshot.clone();
                if reader_output
                    .send((
                        response(
                            request.request_id,
                            WorkerResponse::failed(code, code),
                            snapshot,
                        ),
                        None,
                    ))
                    .is_err()
                {
                    break;
                }
                continue;
            }
            if requests.send(request).is_err() {
                break;
            }
            if terminal {
                wake_wait(&reader_state);
            }
        }
        reader_state.lock().unwrap().closing = true;
        wake_wait(&reader_state);
    });
    let control_state = shared.clone();
    let session_id = args.session_id.clone();
    thread::spawn(move || {
        while let Ok(line) = read_jsonl_line(&mut control_pipe) {
            let Ok(request) = decode_jsonl::<WorkerControlRequest>(&line) else {
                break;
            };
            let response = control_request(&control_state, &session_id, request.message);
            let Ok(line) = encode_jsonl(&WorkerEnvelope::new(request.request_id, response)) else {
                break;
            };
            if control_pipe
                .write_all(line.as_bytes())
                .and_then(|_| control_pipe.flush())
                .is_err()
            {
                break;
            }
        }
    });
    let event_thread = start_events(shared.clone(), output.clone());
    for request in incoming {
        if shared.lock().unwrap().stop {
            break;
        }
        let terminal = matches!(
            request.message,
            WorkerRequest::CloseSession { .. } | WorkerRequest::KillSession { .. }
        );
        let operation = request.message.operation_id().cloned();
        let command_activity = if !terminal {
            worker
                .session
                .as_ref()
                .map(|s| s.begin_activity(ActivityKind::Command))
                .transpose()?
        } else {
            None
        };
        if let Some(epoch) = command_activity {
            shared.lock().unwrap().active = Some(Activity {
                epoch,
                operation: operation.clone(),
                kind: ActivityKind::Command,
            });
            let snapshot = owner_snapshot(&worker, "executing", Some(epoch), None);
            let mut frame = WorkerEnvelope::new("event", WorkerMessage::Event { events: vec![] });
            frame.snapshot = snapshot;
            output.send((frame, None))?;
        }
        let mut result = worker.handle_request(request.message);
        let native_hr = command_activity
            .map(|epoch| worker.session.as_ref().unwrap().end_activity(epoch))
            .transpose()?;
        if native_hr == Some(0xd000013au32 as i32) {
            if let WorkerResponse::Failed { code, .. } = &mut result {
                if code == "operation_failed" {
                    *code = "execution_canceled".into();
                }
            }
        }
        if let Some(epoch) = command_activity {
            let dir = shared.lock().unwrap().artifact_dir.clone();
            if let Some(dir) = dir {
                let error=append_event(&dir.join("engine-results.jsonl"),&serde_json::json!({"observed_at":Timestamp::now(),"epoch":epoch,"operation_id":operation,"native_hresult":native_hr})).err().map(|e|e.to_string());
                if let Some(snapshot) = shared.lock().unwrap().snapshot.as_mut() {
                    snapshot.context["engine"]["result_log_error"] = serde_json::json!(error);
                }
            }
        }
        shared.lock().unwrap().active = None;
        let should_exit = terminal && matches!(result, WorkerResponse::Ok { .. });
        let running = !worker.observation_only
            && worker
                .session
                .as_ref()
                .and_then(|s| s.engine_state().ok())
                .is_some_and(|s| s.target_state == 1);
        let wait_epoch = {
            let mut state = shared.lock().unwrap();
            let epoch = if running && !state.closing {
                let epoch = worker
                    .session
                    .as_ref()
                    .unwrap()
                    .begin_activity(ActivityKind::EventWait)?;
                state.active = Some(Activity {
                    epoch,
                    operation: None,
                    kind: ActivityKind::EventWait,
                });
                Some(epoch)
            } else {
                None
            };
            state.reserved = false;
            if terminal && !should_exit {
                state.closing = false;
            }
            epoch
        };
        let snapshot = owner_snapshot(
            &worker,
            if wait_epoch.is_some() {
                "waiting_for_event"
            } else {
                "idle"
            },
            wait_epoch,
            native_hr,
        );
        let (ack, received) = mpsc::sync_channel(1);
        output.send((
            response(request.request_id, result, snapshot),
            should_exit.then_some(ack),
        ))?;
        if should_exit {
            let _ = received.recv_timeout(Duration::from_secs(3));
            break;
        }
        if let Some(epoch) = wait_epoch {
            let session = worker.session.as_ref().unwrap();
            let wait_result = session.wait_event(epoch)?;
            session.end_activity(epoch)?;
            shared.lock().unwrap().active = None;
            let snapshot = if wait_result == 0x8000000au32 as i32 {
                // EXIT can leave live execution running behind a synthetic BREAK.
                // Do not query thread/scope APIs in this transient engine state.
                let mut state = shared.lock().unwrap();
                if let Some(snapshot) = state.snapshot.as_mut() {
                    if snapshot.target_state != dbgatlas_model::TargetState::Offline {
                        snapshot.target_state = dbgatlas_model::TargetState::Unknown;
                    }
                    snapshot.context["engine"]["state"] = serde_json::json!("idle");
                    snapshot.context["engine"]["epoch"] = serde_json::Value::Null;
                    snapshot.context["engine"]["native_hresult"] = serde_json::json!(wait_result);
                }
                stamp(&mut state)
            } else {
                owner_snapshot(&worker, "idle", None, Some(wait_result))
            };
            let mut frame = WorkerEnvelope::new(
                "event",
                WorkerMessage::Event {
                    events: vec![
                        serde_json::json!({"event":"wait_completed","epoch":epoch,"native_hresult":wait_result}),
                    ],
                },
            );
            frame.snapshot = snapshot;
            output.send((frame, None))?;
        }
    }
    shared.lock().unwrap().stop = true;
    let _ = event_thread.join();
    drop(output);
    // The request reader may still hold a sender while the peer handles close.
    // All terminal responses have already been flushed; process exit closes it.
    drop(writer_thread);
    Ok(())
}
