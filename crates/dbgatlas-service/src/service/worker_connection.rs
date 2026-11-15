use super::*;
use dbgatlas_worker_protocol::WorkerMessage;
use std::sync::mpsc::{self, SyncSender};

type PendingResponse = Result<WorkerEnvelope<WorkerResponse>, String>;

/// One reader drains responses and observations for the entire worker lifetime.
/// No lock is held while a caller waits for its response.
pub(super) struct WorkerConnection {
    writer: Mutex<PipeStream>,
    pending: Arc<Mutex<HashMap<String, SyncSender<PendingResponse>>>>,
    closed: Arc<AtomicBool>,
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;

    #[test]
    fn late_responses_events_and_eof_are_dispatched_independently() {
        let session = SessionRef::new(Id::new("pipe-dispatch-test").unwrap());
        let name = unique_pipe_name(&session);
        let server = WorkerTransport::create_server(&name).unwrap();
        let client_name = name.clone();
        let client = std::thread::spawn(move || {
            let file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OVERLAPPED)
                .open(client_name)
                .unwrap();
            let mut writer = PipeStream::from_file(file);
            let mut reader = std::io::BufReader::new(writer.try_clone().unwrap());
            let first =
                decode_jsonl::<WorkerRequest>(&read_jsonl_line(&mut reader).unwrap()).unwrap();
            // A successor must be writable while the dedicated reader is pending.
            let second =
                decode_jsonl::<WorkerRequest>(&read_jsonl_line(&mut reader).unwrap()).unwrap();
            let snapshot = |sequence| dbgatlas_worker_protocol::WorkerSnapshot {
                sequence,
                target_process: None,
                target_state: dbgatlas_model::TargetState::Stopped,
                observed_at: Timestamp::now(),
                context: json!({"sequence":sequence}),
            };
            let mut event = WorkerEnvelope::new("event", WorkerMessage::Event { events: vec![] });
            event.snapshot = Some(snapshot(3));
            writer
                .write_all(encode_jsonl(&event).unwrap().as_bytes())
                .unwrap();
            for (id, sequence, label) in [
                (second.request_id, 2, "second"),
                (first.request_id, 1, "late-first"),
            ] {
                let mut frame = WorkerEnvelope::new(
                    id,
                    WorkerMessage::Response {
                        result: WorkerResponse::ok(label),
                    },
                );
                frame.snapshot = Some(snapshot(sequence));
                writer
                    .write_all(encode_jsonl(&frame).unwrap().as_bytes())
                    .unwrap();
            }
            // Closing with another request outstanding must unblock its caller.
            read_jsonl_line(&mut reader).unwrap();
        });
        let transport = server.connect(3000).unwrap();
        let snapshot = transport.snapshot.clone();
        let connection = WorkerConnection::start(transport).unwrap();
        let worker = WorkerHandle {
            worker_id: Id::new("pipe-test-worker").unwrap(),
            session_id: session.clone(),
            pipe_name: name,
            identity: WorkerIdentity::CurrentUserDevMode,
        };
        let request = || WorkerRequest::CloseSession {
            session_id: session.clone(),
        };
        assert!(
            connection
                .request(&worker, request(), Some(Duration::from_millis(20)))
                .is_err()
        );
        assert_eq!(
            connection
                .request(&worker, request(), Some(Duration::from_secs(3)))
                .unwrap(),
            WorkerResponse::ok("second")
        );
        assert_eq!(snapshot.lock().unwrap().as_ref().unwrap().sequence, 3);
        assert!(
            connection
                .request(&worker, request(), Some(Duration::from_secs(3)))
                .is_err()
        );
        client.join().unwrap();
    }
}

impl WorkerConnection {
    pub(super) fn start(transport: WorkerTransport) -> Result<Self, ServiceError> {
        let mut reader = std::io::BufReader::new(transport.file.try_clone()?);
        let pending = Arc::new(Mutex::new(
            HashMap::<String, SyncSender<PendingResponse>>::new(),
        ));
        let closed = Arc::new(AtomicBool::new(false));
        let requests = pending.clone();
        let reader_closed = closed.clone();
        let snapshot = transport.snapshot;
        std::thread::spawn(move || {
            let result = (|| -> Result<(), ServiceError> {
                loop {
                    let line = read_jsonl_line(&mut reader)?;
                    let frame: WorkerEnvelope<WorkerMessage> = decode_jsonl(&line)?;
                    if let Some(observation) = frame.snapshot.as_ref() {
                        let mut current = snapshot
                            .lock()
                            .map_err(|_| ServiceError::Worker("snapshot lock poisoned".into()))?;
                        if current
                            .as_ref()
                            .is_none_or(|old| observation.sequence > old.sequence)
                        {
                            *current = Some(observation.clone());
                        }
                    }
                    if let WorkerMessage::Response { result } = frame.message {
                        let sender = requests
                            .lock()
                            .map_err(|_| ServiceError::Worker("response registry poisoned".into()))?
                            .remove(&frame.request_id);
                        if let Some(sender) = sender {
                            let _ = sender.try_send(Ok(WorkerEnvelope {
                                version: frame.version,
                                request_id: frame.request_id,
                                snapshot: frame.snapshot,
                                message: result,
                            }));
                        }
                        // A timed-out observer may have gone away. Never hand its late
                        // response to the next request; cached observations still apply.
                    }
                }
            })();
            reader_closed.store(true, Ordering::SeqCst);
            let reason = result
                .err()
                .map(|e| e.to_string())
                .unwrap_or_else(|| "worker connection closed".into());
            if let Ok(mut requests) = requests.lock() {
                for (_, sender) in requests.drain() {
                    let _ = sender.try_send(Err(reason.clone()));
                }
            }
        });
        Ok(Self {
            writer: Mutex::new(transport.file),
            pending,
            closed,
        })
    }

    pub(super) fn request(
        &self,
        worker: &WorkerHandle,
        request: WorkerRequest,
        timeout: Option<Duration>,
    ) -> Result<WorkerResponse, ServiceError> {
        let id = next_worker_request_id();
        let method = request.method_name();
        let session = request.session_id().to_string();
        let operation = request.operation_id().map(ToString::to_string);
        let started = Instant::now();
        let line = encode_jsonl(&WorkerEnvelope::new(id.clone(), request))?;
        let (sender, receiver) = mpsc::sync_channel(1);
        {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| ServiceError::Worker("response registry poisoned".into()))?;
            if self.closed.load(Ordering::SeqCst) {
                return Err(ServiceError::Worker("worker connection is closed".into()));
            }
            pending.insert(id.clone(), sender);
        }
        let result = (|| {
            {
                let mut writer = self
                    .writer
                    .lock()
                    .map_err(|_| ServiceError::Worker("worker writer poisoned".into()))?;
                writer.write_all(line.as_bytes())?;
                writer.flush()?;
            }
            let response = match timeout {
                Some(timeout) => receiver.recv_timeout(timeout).map_err(|e| {
                    ServiceError::Worker(format!("worker response wait ended: {e}"))
                })?,
                None => receiver
                    .recv()
                    .map_err(|e| ServiceError::Worker(e.to_string()))?,
            }
            .map_err(ServiceError::Worker)?;
            Ok(response.message)
        })();
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&id);
        }
        append_worker_transport_log(
            "complete",
            worker,
            method,
            &id,
            &session,
            operation.as_deref(),
            result.as_ref().ok().map(worker_response_kind),
            result.as_ref().err().map(ToString::to_string).as_deref(),
            started.elapsed(),
        );
        result
    }
}
