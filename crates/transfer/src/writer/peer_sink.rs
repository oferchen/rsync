//! A server's live diagnostic sink: each line becomes a `MSG_*` frame.
//!
//! upstream: log.c:357-366 rwrite() hands every server-side message to
//! `send_msg()` the moment it is produced, after `logit()` has written it to a
//! daemon's log (log.c:312-332). Lines may come from any thread of the
//! session (the receiver, the disk-commit thread, a rayon worker), so the sink
//! queues the frame in a [`PeerOutbox`] and the connection's multiplex writer
//! sends the queue, in production order, before the next bytes it writes.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use logging::{DiagnosticEvent, LogCode, LogContext, Stamped};
use protocol::MessageCode;

use crate::progress::SharedDaemonLog;

/// Frames waiting for the connection writer, oldest first.
#[derive(Debug, Default)]
pub(crate) struct PeerOutbox {
    frames: Mutex<VecDeque<(MessageCode, Vec<u8>)>>,
    /// Lets the writer's hot path skip the lock when nothing is queued.
    pending: AtomicBool,
}

impl PeerOutbox {
    fn push(&self, code: MessageCode, payload: Vec<u8>) {
        let mut frames = self.frames.lock().unwrap_or_else(PoisonError::into_inner);
        frames.push_back((code, payload));
        self.pending.store(true, Ordering::Release);
    }

    /// Takes every queued frame, or `None` when the queue is empty.
    #[inline]
    pub(crate) fn has_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }

    pub(crate) fn take(&self) -> Option<VecDeque<(MessageCode, Vec<u8>)>> {
        if !self.pending.load(Ordering::Acquire) {
            return None;
        }
        let mut frames = self.frames.lock().unwrap_or_else(PoisonError::into_inner);
        self.pending.store(false, Ordering::Release);
        Some(std::mem::take(&mut *frames))
    }
}

/// Turns each diagnostic into the frame upstream's `rwrite()` would send.
struct PeerSink {
    outbox: Arc<PeerOutbox>,
    daemon_log: Option<SharedDaemonLog>,
}

impl PeerSink {
    /// The frame for `code`, or `None` for a line the peer never sees.
    ///
    /// upstream: log.c:303-308 folds FERROR_SOCKET and FERROR_UTF8 into
    /// FERROR; log.c:344-346 drops FINFO under `--quiet`. The writer routes
    /// the frame by msgs2stderr and remaps it for a pre-30 peer
    /// (log.c:357-366, [`super::frame_code`]).
    fn frame_code(code: LogCode) -> Option<MessageCode> {
        match code {
            LogCode::ErrorXfer => Some(MessageCode::ErrorXfer),
            LogCode::Error | LogCode::ErrorSocket | LogCode::ErrorUtf8 => Some(MessageCode::Error),
            LogCode::Warning => Some(MessageCode::Warning),
            LogCode::Info if logging::finfo_suppressed() => None,
            LogCode::Info | LogCode::Client => Some(MessageCode::Info),
            LogCode::Log | LogCode::None => None,
        }
    }
}

impl logging::DiagnosticSink for PeerSink {
    fn write(&self, event: &Stamped<DiagnosticEvent>) {
        let event = event.value();
        let code = event.code();
        let message = match event {
            DiagnosticEvent::Info { message, .. } | DiagnosticEvent::Debug { message, .. } => {
                message.as_bytes()
            }
            DiagnosticEvent::Bytes { message, .. } => message.as_slice(),
        };
        // upstream: log.c:312-332 - a daemon logs every code but FNONE, and
        // writes the log line before the frame goes out.
        if code != LogCode::None
            && let Some(log) = &self.daemon_log
        {
            log.write(message);
        }
        if let Some(frame) = Self::frame_code(code) {
            let mut payload = Vec::with_capacity(message.len() + 1);
            payload.extend_from_slice(message);
            payload.push(b'\n');
            self.outbox.push(frame, payload);
        }
    }
}

thread_local! {
    /// The outbox of the server session running on this thread, picked up by
    /// the multiplex writer it creates.
    static SESSION_OUTBOX: RefCell<Option<Arc<PeerOutbox>>> = const { RefCell::new(None) };
}

/// The outbox a multiplex writer created on this thread must send, if this
/// thread serves a session.
pub(crate) fn session_outbox() -> Option<Arc<PeerOutbox>> {
    SESSION_OUTBOX.with(|outbox| outbox.borrow().clone())
}

/// Routes a server session's diagnostics to its peer until dropped.
pub(crate) struct PeerDiagnostics {
    context: Arc<LogContext>,
}

impl PeerDiagnostics {
    /// Installs the peer sink on the session's log context. Lines produced
    /// before the multiplexed stream opens wait in the outbox for it.
    pub(crate) fn install(daemon_log: Option<SharedDaemonLog>) -> Self {
        let outbox = Arc::new(PeerOutbox::default());
        SESSION_OUTBOX.with(|slot| *slot.borrow_mut() = Some(Arc::clone(&outbox)));
        let context = logging::current();
        context.install_sink(Arc::new(PeerSink { outbox, daemon_log }));
        Self { context }
    }
}

impl Drop for PeerDiagnostics {
    fn drop(&mut self) {
        self.context.remove_sink();
        SESSION_OUTBOX.with(|slot| *slot.borrow_mut() = None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sink() -> (PeerSink, Arc<PeerOutbox>) {
        let outbox = Arc::new(PeerOutbox::default());
        let sink = PeerSink {
            outbox: Arc::clone(&outbox),
            daemon_log: None,
        };
        (sink, outbox)
    }

    fn info(code: LogCode, text: &str) -> Stamped<DiagnosticEvent> {
        Stamped::stamp(DiagnosticEvent::Info {
            flag: logging::InfoFlag::Misc,
            level: 0,
            code,
            message: text.to_owned(),
        })
    }

    #[test]
    fn each_line_is_queued_as_its_frame_in_production_order() {
        let (sink, outbox) = sink();
        use logging::DiagnosticSink as _;
        sink.write(&info(LogCode::Info, "first"));
        sink.write(&info(LogCode::Warning, "second"));
        sink.write(&info(LogCode::Log, "log only"));
        let frames: Vec<_> = outbox.take().unwrap().into_iter().collect();
        assert_eq!(
            frames,
            vec![
                (MessageCode::Info, b"first\n".to_vec()),
                (MessageCode::Warning, b"second\n".to_vec()),
            ]
        );
        assert!(outbox.take().is_none());
    }

    /// upstream: log.c:303-308 - the socket and UTF-8 error codes travel as
    /// FERROR; the queue keeps upstream's code and leaves the pre-30 remap to
    /// the writer, so it happens in one place.
    #[test]
    fn the_queue_keeps_upstreams_codes() {
        assert_eq!(
            PeerSink::frame_code(LogCode::ErrorSocket),
            Some(MessageCode::Error)
        );
        assert_eq!(
            PeerSink::frame_code(LogCode::ErrorUtf8),
            Some(MessageCode::Error)
        );
        assert_eq!(
            PeerSink::frame_code(LogCode::Warning),
            Some(MessageCode::Warning)
        );
        assert_eq!(PeerSink::frame_code(LogCode::Log), None);
    }
}
