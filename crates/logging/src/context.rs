//! Session-scoped verbosity and the live diagnostic sink.
//!
//! Upstream rsync keeps `info_levels[]`, `debug_levels[]` and `quiet` as
//! process globals (upstream: options.c:263, rsync.h:1473) and writes every
//! message the moment it is produced: `rprintf()` formats it and `rwrite()`
//! hands it to stdout, stderr, the log file or a `MSG_*` frame
//! (upstream: log.c:272-373). oc-rsync runs its transfer roles as threads, and
//! a QUIC daemon serves several sessions inside one process, so the
//! equivalent state lives in a [`LogContext`] shared by every thread of one
//! session:
//!
//! - verbosity and `quiet`, read by [`info_gte`], [`debug_gte`] and
//!   [`finfo_suppressed`] - the `INFO_GTE()`/`DEBUG_GTE()` macros
//!   (upstream: rsync.h:1475-1477);
//! - the sink every `emit_*` call writes through at once, as `rwrite()` does.
//!
//! A thread uses the context it [`enter`]ed, or the process default. A process
//! that serves one session (the client, an rsh or forked-daemon server) needs
//! no `enter` at all: its disk-commit and rayon threads see the same levels and
//! the same sink as the thread that set them.
//!
//! Until a sink is installed, events are held in the context's buffer in
//! production order. The in-process local copy renders that buffer with its
//! post-hoc listing, and tests read it back through the `drain_*` functions.

use super::config::VerbosityConfig;
use super::levels::{DebugFlag, InfoFlag};
use super::log_code::LogCode;
use super::sequence::Stamped;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, PoisonError, RwLock};


/// A diagnostic event collected during execution.
///
/// The two variants correspond to the info and debug flag systems. Every
/// event additionally carries the upstream [`LogCode`] that classifies its
/// destination (upstream: log.c:rwrite() dispatches on `enum logcode`).
#[derive(Clone, Debug)]
pub enum DiagnosticEvent {
    /// Info-level diagnostic event.
    Info {
        /// The info flag category.
        flag: InfoFlag,
        /// The verbosity level.
        level: u8,
        /// The upstream log code classifying the event's destination.
        code: LogCode,
        /// The diagnostic message.
        message: String,
    },
    /// Debug-level diagnostic event.
    Debug {
        /// The debug flag category.
        flag: DebugFlag,
        /// The verbosity level.
        level: u8,
        /// The upstream log code classifying the event's destination.
        code: LogCode,
        /// The diagnostic message.
        message: String,
    },
    /// A byte-faithful notice whose message may carry a non-UTF-8 filename.
    ///
    /// Rendered exactly like [`Info`](DiagnosticEvent::Info) - upstream's
    /// `rwrite()` writes an FINFO line the same way regardless of the flag
    /// category - but the message survives to the render boundary as raw
    /// bytes rather than a `String`. This mirrors upstream `rwrite()`, whose
    /// buffer is a `char *` copied to the output fd verbatim (log.c:425
    /// `rwrite()` -> `filtered_fwrite()`), so a filename operand that is not
    /// valid UTF-8 (or that carries a control byte) reaches the escaping sink
    /// unmangled instead of being lossily replaced with U+FFFD during
    /// `format!()`. The escaping itself happens once, at the sink
    /// (log.c:242 `filtered_fwrite`), never here.
    Bytes {
        /// The info flag category.
        flag: InfoFlag,
        /// The verbosity level.
        level: u8,
        /// The upstream log code classifying the event's destination.
        code: LogCode,
        /// The diagnostic message as raw bytes (may be non-UTF-8).
        message: Vec<u8>,
    },
}

impl DiagnosticEvent {
    /// Returns the upstream [`LogCode`] carried by this event.
    #[must_use]
    pub const fn code(&self) -> LogCode {
        match self {
            DiagnosticEvent::Info { code, .. }
            | DiagnosticEvent::Debug { code, .. }
            | DiagnosticEvent::Bytes { code, .. } => *code,
        }
    }
}

/// Destination for diagnostics, written at the moment each is produced.
///
/// The client's sink writes to stdout/stderr, a server's frames the message
/// for its peer. upstream: log.c:272-373 rwrite().
pub trait DiagnosticSink: Send + Sync {
    /// Writes one event. Called from whichever thread produced it.
    fn write(&self, event: &Stamped<DiagnosticEvent>);
}

/// The verbosity, `quiet` flag and diagnostic sink of one session.
pub struct LogContext {
    verbosity: RwLock<VerbosityConfig>,
    quiet: AtomicBool,
    /// Bumped on every verbosity or `quiet` change so threads refresh their
    /// cached copy.
    generation: AtomicU64,
    sink: RwLock<Option<Arc<dyn DiagnosticSink>>>,
    buffer: Mutex<Vec<Stamped<DiagnosticEvent>>>,
    /// Open [`discard_events_from`] scopes while a sink is installed.
    discarding: AtomicU64,
    /// Set once an `FERROR_XFER` line is produced.
    /// upstream: log.c:338 rwrite() `got_xfer_error = 1`.
    xfer_error: AtomicBool,
}

impl std::fmt::Debug for LogContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogContext")
            .field("generation", &self.generation.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Default for LogContext {
    fn default() -> Self {
        Self {
            verbosity: RwLock::new(VerbosityConfig::default()),
            quiet: AtomicBool::new(false),
            generation: AtomicU64::new(1),
            sink: RwLock::new(None),
            buffer: Mutex::new(Vec::new()),
            discarding: AtomicU64::new(0),
            xfer_error: AtomicBool::new(false),
        }
    }
}

impl LogContext {
    /// Creates a context with default verbosity and no sink.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Installs `sink`, first writing every event buffered so far through it,
    /// in production order.
    pub fn install_sink(&self, sink: Arc<dyn DiagnosticSink>) {
        let mut slot = self.sink.write().unwrap_or_else(PoisonError::into_inner);
        let pending: Vec<_> = self.lock_buffer().drain(..).collect();
        for event in &pending {
            sink.write(event);
        }
        *slot = Some(sink);
    }

    /// Removes the installed sink, so later events are buffered again.
    pub fn remove_sink(&self) -> Option<Arc<dyn DiagnosticSink>> {
        self.sink
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// Whether a sink is installed.
    #[must_use]
    pub fn has_sink(&self) -> bool {
        self.sink
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    fn dispatch(&self, event: Stamped<DiagnosticEvent>) {
        if event.value().code() == LogCode::ErrorXfer {
            self.xfer_error.store(true, Ordering::Release);
        }
        let sink = self.sink.read().unwrap_or_else(PoisonError::into_inner);
        match sink.as_ref() {
            Some(sink) => {
                if self.discarding.load(Ordering::Acquire) == 0 {
                    sink.write(&event);
                }
            }
            None => self.lock_buffer().push(event),
        }
    }

    fn lock_buffer(&self) -> std::sync::MutexGuard<'_, Vec<Stamped<DiagnosticEvent>>> {
        self.buffer.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn update_verbosity(&self, f: impl FnOnce(&mut VerbosityConfig)) {
        let mut verbosity = self.verbosity.write().unwrap_or_else(PoisonError::into_inner);
        f(&mut verbosity);
        self.generation.fetch_add(1, Ordering::Release);
    }
}

static DEFAULT_CONTEXT: LazyLock<Arc<LogContext>> = LazyLock::new(LogContext::new);

/// A thread's view of its context, with the levels cached so the hot
/// `info_gte`/`debug_gte` checks cost one relaxed load when nothing changed.
struct ThreadView {
    context: Arc<LogContext>,
    generation: u64,
    verbosity: VerbosityConfig,
    quiet: bool,
}

impl ThreadView {
    fn new(context: Arc<LogContext>) -> Self {
        Self {
            context,
            generation: 0,
            verbosity: VerbosityConfig::default(),
            quiet: false,
        }
    }

    fn refreshed(&mut self) -> &Self {
        let generation = self.context.generation.load(Ordering::Acquire);
        if generation != self.generation {
            self.verbosity = self
                .context
                .verbosity
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            self.quiet = self.context.quiet.load(Ordering::Acquire);
            self.generation = generation;
        }
        self
    }
}

thread_local! {
    static VIEW: RefCell<Option<ThreadView>> = const { RefCell::new(None) };
}

fn with_view<R>(f: impl FnOnce(&ThreadView) -> R) -> R {
    VIEW.with(|view| {
        let mut view = view.borrow_mut();
        let view = view.get_or_insert_with(|| ThreadView::new(Arc::clone(&DEFAULT_CONTEXT)));
        f(view.refreshed())
    })
}

/// The context the calling thread uses: the one it entered, or the process
/// default.
#[must_use]
pub fn current() -> Arc<LogContext> {
    VIEW.with(|view| {
        view.borrow()
            .as_ref()
            .map_or_else(|| Arc::clone(&DEFAULT_CONTEXT), |v| Arc::clone(&v.context))
    })
}

/// Makes the calling thread use `context` until the guard drops.
///
/// A thread that serves one session of a multi-session process (the QUIC
/// daemon) enters that session's context; a thread it spawns enters the same
/// one, captured with [`current`] before the spawn.
#[must_use = "the context is left when the guard drops"]
pub fn enter(context: Arc<LogContext>) -> ContextGuard {
    let previous = VIEW.with(|view| view.borrow_mut().replace(ThreadView::new(context)));
    ContextGuard { previous }
}

/// Restores the previously used context when dropped. See [`enter`].
pub struct ContextGuard {
    previous: Option<ThreadView>,
}

impl Drop for ContextGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        VIEW.with(|view| *view.borrow_mut() = previous);
    }
}

/// The session a thread entered, carried into work it hands to a shared pool.
///
/// A rayon worker serves every session in the process, so it has no session of
/// its own: a job that logs must enter the session that queued it, or a QUIC
/// session's lines would reach the daemon-wide context. Capture before handing
/// work to the pool and [`enter`](Self::enter) at the top of each job. A thread
/// on the process default captures nothing, so entering costs nothing there.
#[derive(Clone, Debug, Default)]
pub struct Session(Option<Arc<LogContext>>);

impl Session {
    /// Captures the calling thread's entered context, if it has one.
    #[must_use]
    pub fn capture() -> Self {
        Self(VIEW.with(|view| {
            view.borrow()
                .as_ref()
                .filter(|view| !Arc::ptr_eq(&view.context, &DEFAULT_CONTEXT))
                .map(|view| Arc::clone(&view.context))
        }))
    }

    /// Makes the calling thread use the captured context until the guard drops.
    #[must_use = "the context is left when the guard drops"]
    pub fn enter(&self) -> Option<ContextGuard> {
        self.0.as_ref().map(|context| enter(Arc::clone(context)))
    }
}

/// Installs the verbosity configuration of the calling thread's context.
///
/// Every thread of the session sees the new levels, as every upstream role
/// reads the same `info_levels[]` (upstream: options.c:263).
pub fn init(config: VerbosityConfig) {
    current().update_verbosity(|verbosity| *verbosity = config);
}

/// Records whether `-q` / `--quiet` was requested.
///
/// Upstream keeps `quiet` as a global of its own, separate from `verbose` and
/// from `info_levels[]`, and consults it in exactly one place: the `FINFO` arm
/// of `rwrite()` returns without writing (upstream: log.c:344-345). Anything
/// that only folds `--quiet` into `verbose = 0` cannot express a notice
/// upstream prints at default verbosity and suppresses only under `-q` - the
/// two states become indistinguishable.
pub fn set_quiet(quiet: bool) {
    let context = current();
    context.quiet.store(quiet, Ordering::Release);
    context.generation.fetch_add(1, Ordering::Release);
}

/// Whether `FINFO` output is suppressed.
///
/// The single question a producer or renderer should ask before emitting an
/// upstream `rprintf(FINFO, ...)` that carries no `INFO_GTE` gate of its own.
/// upstream: log.c:345 rwrite() `if (quiet)` - the FINFO arm's early return.
#[must_use]
pub fn finfo_suppressed() -> bool {
    with_view(|view| view.quiet)
}

/// Check if the info flag is at or above the specified level.
///
/// Equivalent to upstream's `INFO_GTE(flag, level)` macro (upstream: rsync.h:1475).
pub fn info_gte(flag: InfoFlag, level: u8) -> bool {
    with_view(|view| view.verbosity.info.get(flag) >= level)
}

/// Check if the debug flag is at or above the specified level.
///
/// Equivalent to upstream's `DEBUG_GTE(flag, level)` macro (upstream: rsync.h:1477).
pub fn debug_gte(flag: DebugFlag, level: u8) -> bool {
    with_view(|view| view.verbosity.debug.get(flag) >= level)
}

fn emit(event: DiagnosticEvent) {
    current().dispatch(Stamped::stamp(event));
}

/// Emit an info diagnostic event.
///
/// The caller is responsible for checking [`info_gte`] first; the
/// [`info_log!`](crate::info_log) macro handles this automatically.
///
/// The event is tagged [`LogCode::Info`], upstream's `FINFO` (client stdout,
/// subject to verbosity). Producers that mirror a different upstream
/// `rprintf()` code use [`emit_info_coded`].
/// upstream: log.c:rwrite() dispatches FINFO messages
pub fn emit_info(flag: InfoFlag, level: u8, message: String) {
    emit_info_coded(flag, level, LogCode::Info, message);
}

/// Emit an info diagnostic event carrying an explicit upstream [`LogCode`].
///
/// Use this when the producing site mirrors an upstream `rprintf(code, ...)`
/// whose code is not `FINFO` (upstream: log.c:rwrite() dispatches on the
/// code to pick the client stream versus the log file).
pub fn emit_info_coded(flag: InfoFlag, level: u8, code: LogCode, message: String) {
    emit(DiagnosticEvent::Info {
        flag,
        level,
        code,
        message,
    });
}

/// Emit a byte-faithful info notice.
///
/// This is the byte-capable sibling of [`emit_info`]: the message is carried
/// as raw bytes so a filename operand that is not valid UTF-8, or that holds a
/// control byte, reaches the sink intact rather than being replaced with
/// U+FFFD during `format!()`. The caller is responsible for checking
/// [`info_gte`] first; the [`info_log_bytes!`](crate::info_log_bytes) macro
/// handles this automatically.
///
/// The event is tagged [`LogCode::Info`], matching [`emit_info`]. The escaping
/// of the bytes happens once, at the sink (upstream: log.c:242
/// `filtered_fwrite`), never at this emitter.
pub fn emit_info_bytes(flag: InfoFlag, level: u8, message: Vec<u8>) {
    emit(DiagnosticEvent::Bytes {
        flag,
        level,
        code: LogCode::Info,
        message,
    });
}

/// Emit a warning diagnostic, unconditionally.
///
/// A warning is not gated on verbosity: upstream's `FWARNING` is a
/// first-class log code that `rwrite()` dispatches on its own
/// (`log.c:341`), and it is carried over the socket for protocols >= 30
/// (`rsync.h:285`, `rsync.h:296` `MSG_WARNING = FWARNING`). Emitting it at
/// `level` 0 keeps [`info_gte`] trivially true, so the message is produced
/// whatever the operator passed for `-v`.
///
/// The event carries [`LogCode::Warning`], which the stream router sends to
/// stderr (see `stream.rs`). The
/// `rsync warning: … (code N) at file(line)` trailer upstream renders at
/// `log.c:956` is deliberately not applied here; that formatting belongs to
/// the single logcode-to-stream funnel, not to the emitter.
/// upstream: log.c:341 rwrite() `case FWARNING:`
pub fn emit_warning(message: String) {
    emit_info_coded(InfoFlag::Misc, 0, LogCode::Warning, message);
}

/// Emit a debug diagnostic event.
///
/// The caller is responsible for checking [`debug_gte`] first; the
/// [`debug_log!`](crate::debug_log) macro handles this automatically.
///
/// The event is tagged [`LogCode::Info`]: upstream debug traces are emitted
/// through `rprintf(FINFO, ...)` and share FINFO routing. Producers that
/// mirror a different upstream code use [`emit_debug_coded`].
/// upstream: log.c:rwrite() dispatches FINFO debug messages
pub fn emit_debug(flag: DebugFlag, level: u8, message: String) {
    emit_debug_coded(flag, level, LogCode::Info, message);
}

/// Emit a debug diagnostic event carrying an explicit upstream [`LogCode`].
///
/// Use this when the producing site mirrors an upstream `rprintf(code, ...)`
/// whose code is not `FINFO` (upstream: log.c:rwrite() dispatches on the
/// code to pick the client stream versus the log file).
pub fn emit_debug_coded(flag: DebugFlag, level: u8, code: LogCode, message: String) {
    emit(DiagnosticEvent::Debug {
        flag,
        level,
        code,
        message,
    });
}

/// Whether this session has produced an `FERROR_XFER` line, which makes a
/// finished run exit with code 23.
///
/// upstream: log.c:338 rwrite() sets `got_xfer_error` for every such line,
/// whichever stream it then goes to.
#[must_use]
pub fn xfer_error_reported() -> bool {
    current().xfer_error.load(Ordering::Acquire)
}

/// Drain every buffered event with its production order, clearing the buffer.
///
/// This is the drain for a consumer that has to interleave these events with
/// another output channel: the key says which of two messages was produced
/// first.
pub fn drain_stamped_events() -> Vec<Stamped<DiagnosticEvent>> {
    current().lock_buffer().drain(..).collect()
}

/// Drain every buffered event, clearing the buffer. Returns events in
/// production order.
pub fn drain_events() -> Vec<DiagnosticEvent> {
    drain_stamped_events()
        .into_iter()
        .map(Stamped::into_value)
        .collect()
}

/// Runs `f` and discards every diagnostic event emitted while it runs.
///
/// For a pass whose side effects are not part of the run the user sees, such
/// as the local copy's dry-run preview that sizes `--progress`: upstream makes
/// no such pass, so each notice it would repeat must reach the output once.
/// Events buffered before `f` runs are kept.
pub fn discard_events_from<R>(f: impl FnOnce() -> R) -> R {
    let context = current();
    context.discarding.fetch_add(1, Ordering::AcqRel);
    let mark = context.lock_buffer().len();
    let result = f();
    context.lock_buffer().truncate(mark);
    context.discarding.fetch_sub(1, Ordering::AcqRel);
    result
}

/// Drain the buffered events whose code `accept` selects, leaving the rest.
fn drain_stamped_events_where(accept: impl Fn(LogCode) -> bool) -> Vec<Stamped<DiagnosticEvent>> {
    let context = current();
    let mut events = context.lock_buffer();
    let mut matched = Vec::new();
    events.retain(|stamped| {
        if accept(stamped.value().code()) {
            matched.push(stamped.clone());
            false
        } else {
            true
        }
    });
    matched
}

/// A projection of [`drain_stamped_events_where`] for the consumers that render
/// their share of the buffer on its own, where the key would be noise.
fn drain_events_where(accept: impl Fn(LogCode) -> bool) -> Vec<DiagnosticEvent> {
    drain_stamped_events_where(accept)
        .into_iter()
        .map(Stamped::into_value)
        .collect()
}

/// Drain the events bound for the client stream, leaving `FLOG` queued.
///
/// This is the client renderer's drain, and it carries the production key
/// because that renderer has something to interleave against: the buffered
/// per-file event stream, which it emits post-hoc. `FLOG` is left behind
/// because upstream's `rwrite()` writes a log message and returns before the
/// client-stream dispatch (upstream: log.c:312-334) - the log-file sink takes
/// those through [`drain_events_coded`], and draining them here would consume
/// them before that sink ever runs.
pub fn drain_stamped_events_for_client() -> Vec<Stamped<DiagnosticEvent>> {
    drain_stamped_events_where(|code| code != LogCode::Log)
}

/// Drain only the events carrying `code`, leaving all other events queued.
///
/// This is how a log-file sink consumes `FLOG`-classified events without
/// disturbing the client-stream events that a later [`drain_events`] renders:
/// upstream's `rwrite()` writes an FLOG message to the log file and returns
/// before the client-stream dispatch (upstream: log.c:312-334), so the two
/// destinations drain independently. Returns matches in emission order.
pub fn drain_events_coded(code: LogCode) -> Vec<DiagnosticEvent> {
    drain_events_where(|candidate| candidate == code)
}

/// Drain the events a server must frame to its peer, leaving the rest queued.
///
/// A server does not print these itself: upstream's `rwrite()` hands them to
/// `send_msg()` and returns (upstream: log.c:357-366), so the message travels
/// as a `MSG_*` frame and the client renders it. Without this drain the events
/// stay in the buffer and die with the thread.
///
/// Takes every code upstream's stream switch lets through to that block:
///
/// - `FERROR_XFER`, `FERROR`, `FWARNING` (upstream: log.c:336-342).
/// - `FERROR_SOCKET` and `FERROR_UTF8`, which normalise to `FERROR` first
///   (upstream: log.c:303-308).
/// - `FINFO`, unless `--quiet` is in effect: its arm returns early under
///   `quiet` (upstream: log.c:344-346), before the `send_msg()`. Every
///   `--info` and `--debug` line is an `FINFO` upstream (`rprintf(FINFO, ...)`),
///   and [`emit_debug`] tags debug traces with [`LogCode::Info`] for that
///   reason, so a server's debug output reaches the client the same way.
///
/// Under `--quiet` the `FINFO` events are discarded rather than left queued:
/// upstream drops them at the same point, and a leftover would surface later
/// through another drain. `FLOG` is excluded because upstream returns before
/// the switch (upstream: log.c:331-334); it belongs to [`drain_events_coded`].
/// upstream: log.c:357-366 rwrite() `if (am_server ...) send_msg(msg, ...)`
pub fn drain_events_for_peer() -> Vec<DiagnosticEvent> {
    let quiet = finfo_suppressed();
    let mut events = drain_events_where(|code| is_error_class(code) || code == LogCode::Info);
    if quiet {
        events.retain(|event| event_code(event) != LogCode::Info);
    }
    events
}

/// Drain only the error and warning events, leaving `FINFO` and the rest.
///
/// The receiver uses this when it is the client (a pull): those codes go to
/// its own stderr as they arise (upstream: log.c:336-342 picks stderr), while
/// its `FINFO` output stays queued for the client's summary renderer.
pub fn drain_error_events() -> Vec<DiagnosticEvent> {
    drain_events_where(is_error_class)
}

/// The codes upstream's `rwrite()` sends to stderr (upstream: log.c:303-308,
/// 336-342).
fn is_error_class(code: LogCode) -> bool {
    matches!(
        code,
        LogCode::ErrorXfer
            | LogCode::Error
            | LogCode::Warning
            | LogCode::ErrorSocket
            | LogCode::ErrorUtf8
    )
}

/// The [`LogCode`] an event was emitted under.
fn event_code(event: &DiagnosticEvent) -> LogCode {
    match event {
        DiagnosticEvent::Info { code, .. }
        | DiagnosticEvent::Debug { code, .. }
        | DiagnosticEvent::Bytes { code, .. } => *code,
    }
}

/// Drain the events a daemon must write to its log file, leaving the rest.
///
/// Upstream does not pick codes here: once a message reaches
/// `rwrite()`'s `else if (am_daemon || logfile_name)` block it is handed to
/// `logit()` whatever its code, and only the priority differs - `LOG_INFO`
/// for `FINFO`/`FLOG`, `LOG_WARNING` for everything else
/// (upstream: log.c:312-332). `FLOG` is not special-cased *into* the log; it
/// is special-cased *out of* the stream switch that follows
/// (upstream: log.c:331-334).
///
/// So this takes everything except [`LogCode::None`], upstream's `FNONE`,
/// which `rwrite()` is never handed at all (upstream: rsync.h:283 "never
/// sent"). Draining a narrower set would silently discard a daemon-side
/// `FERROR`: the message would be produced, be correct, and never reach the
/// operator.
/// upstream: log.c:312-332 rwrite() `else if (am_daemon || logfile_name)`
pub fn drain_events_for_daemon_log() -> Vec<DiagnosticEvent> {
    drain_events_where(|code| code != LogCode::None)
}

/// Apply an info flag token to the current context's configuration.
///
/// Parses tokens like `"copy2"` or `"del"` and updates the corresponding
/// flag level. This supports the `--info=FLAGS` CLI option
/// (upstream: options.c:parse_output_words()).
pub fn apply_info_flag(token: &str) -> Result<(), String> {
    let mut result = Ok(());
    current().update_verbosity(|verbosity| result = verbosity.apply_info_flag(token));
    result
}

/// Clamps the current context's info and debug levels to what `level` `-v`
/// flags would set, so explicitly requested levels cannot exceed the cap.
/// A `level` above [`MAX_VERBOSITY`](crate::MAX_VERBOSITY) applies no limit.
///
/// upstream: options.c:546-572 limit_output_verbosity()
pub fn limit_output_verbosity(level: u8) {
    if level > crate::MAX_VERBOSITY {
        return;
    }
    let limit = VerbosityConfig::from_verbose_level(level);
    current().update_verbosity(|verbosity| verbosity.limit_to(&limit));
}

/// Apply a debug flag token to the current context's configuration.
///
/// Parses tokens like `"recv2"` or `"flist"` and updates the corresponding
/// flag level. This supports the `--debug=FLAGS` CLI option
/// (upstream: options.c:parse_output_words()).
pub fn apply_debug_flag(token: &str) -> Result<(), String> {
    let mut result = Ok(());
    current().update_verbosity(|verbosity| result = verbosity.apply_debug_flag(token));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_and_check() {
        let mut config = VerbosityConfig::default();
        config.info.copy = 2;
        config.debug.recv = 3;

        init(config);

        assert!(info_gte(InfoFlag::Copy, 1));
        assert!(info_gte(InfoFlag::Copy, 2));
        assert!(!info_gte(InfoFlag::Copy, 3));

        assert!(debug_gte(DebugFlag::Recv, 1));
        assert!(debug_gte(DebugFlag::Recv, 3));
        assert!(!debug_gte(DebugFlag::Recv, 4));
    }

    #[test]
    fn test_emit_and_drain() {
        init(VerbosityConfig::default());

        emit_info(InfoFlag::Copy, 1, "test info".to_owned());
        emit_debug(DebugFlag::Recv, 2, "test debug".to_owned());

        let events = drain_events();
        assert_eq!(events.len(), 2);

        match &events[0] {
            DiagnosticEvent::Info {
                flag,
                level,
                message,
                ..
            } => {
                assert_eq!(*flag, InfoFlag::Copy);
                assert_eq!(*level, 1);
                assert_eq!(message, "test info");
            }
            _ => panic!("expected info event"),
        }

        match &events[1] {
            DiagnosticEvent::Debug {
                flag,
                level,
                message,
                ..
            } => {
                assert_eq!(*flag, DebugFlag::Recv);
                assert_eq!(*level, 2);
                assert_eq!(message, "test debug");
            }
            _ => panic!("expected debug event"),
        }

        assert_eq!(drain_events().len(), 0);
    }

    /// The behaviour-preserving default: every event emitted through the
    /// legacy emitters (and therefore through `info_log!`/`debug_log!`) is
    /// tagged FINFO, the code whose upstream routing - client stdout,
    /// subject to verbosity (upstream: log.c:344-347) - matches how the
    /// renderer already treats all drained events.
    #[test]
    fn default_emitters_tag_events_finfo() {
        init(VerbosityConfig::default());
        drain_events();

        emit_info(InfoFlag::Copy, 1, "info default".to_owned());
        emit_debug(DebugFlag::Recv, 1, "debug default".to_owned());

        let events = drain_events();
        assert_eq!(events.len(), 2);
        for event in &events {
            assert_eq!(event.code(), LogCode::Info);
        }
    }

    /// Coded emitters attach the producer's real upstream logcode, e.g. FLOG
    /// for log-file-only banners (upstream: log.c:312-334).
    #[test]
    fn coded_emitters_carry_explicit_logcode() {
        init(VerbosityConfig::default());
        drain_events();

        emit_info_coded(
            InfoFlag::Flist,
            1,
            LogCode::Log,
            "building file list".to_owned(),
        );
        emit_debug_coded(
            DebugFlag::Recv,
            1,
            LogCode::Client,
            "client-only".to_owned(),
        );

        let events = drain_events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].code(), LogCode::Log);
        assert_eq!(events[1].code(), LogCode::Client);
        assert!(matches!(
            &events[0],
            DiagnosticEvent::Info { code: LogCode::Log, message, .. } if message == "building file list"
        ));
    }

    /// A daemon logs every code it is handed, not just `FLOG`.
    ///
    /// The narrow reading - "the daemon log takes `FLOG`" - is what
    /// `rwrite()`'s trailing `if (code == FLOG) return;` looks like from the
    /// stream switch. Read from the top, `logit()` has already run for every
    /// code that reached the `am_daemon` branch (upstream: log.c:312-332), so
    /// a drain keyed on `FLOG` alone discards a daemon-side `FERROR`: the
    /// message is produced, is correct, and never reaches the operator.
    #[test]
    fn the_daemon_log_takes_every_code_it_is_handed_not_only_flog() {
        init(VerbosityConfig::default());
        drain_events();

        for code in [
            LogCode::Log,
            LogCode::Error,
            LogCode::Warning,
            LogCode::ErrorXfer,
            LogCode::Info,
        ] {
            emit_info_coded(InfoFlag::Misc, 0, code, format!("{code:?}"));
        }
        // `FNONE` is the one code `rwrite()` is never handed
        // (upstream: rsync.h:283 "never sent").
        emit_info_coded(InfoFlag::Misc, 0, LogCode::None, "unsent".to_owned());

        let logged: Vec<LogCode> = drain_events_for_daemon_log()
            .iter()
            .map(DiagnosticEvent::code)
            .collect();
        assert_eq!(
            logged,
            vec![
                LogCode::Log,
                LogCode::Error,
                LogCode::Warning,
                LogCode::ErrorXfer,
                LogCode::Info,
            ]
        );

        // The excluded code is left in the buffer rather than dropped, so a
        // later drain still owns it - and this is what proves the filter is
        // doing something at all.
        let left: Vec<LogCode> = drain_events().iter().map(DiagnosticEvent::code).collect();
        assert_eq!(left, vec![LogCode::None]);
    }

    #[test]
    fn diagnostic_event_clone() {
        let info_event = DiagnosticEvent::Info {
            flag: InfoFlag::Copy,
            level: 1,
            code: LogCode::Info,
            message: "test".to_owned(),
        };
        let cloned = info_event;
        match cloned {
            DiagnosticEvent::Info {
                flag,
                level,
                message,
                ..
            } => {
                assert_eq!(flag, InfoFlag::Copy);
                assert_eq!(level, 1);
                assert_eq!(message, "test");
            }
            _ => panic!("expected info event"),
        }

        let debug_event = DiagnosticEvent::Debug {
            flag: DebugFlag::Bind,
            level: 2,
            code: LogCode::Info,
            message: "debug".to_owned(),
        };
        let cloned = debug_event;
        match cloned {
            DiagnosticEvent::Debug {
                flag,
                level,
                message,
                ..
            } => {
                assert_eq!(flag, DebugFlag::Bind);
                assert_eq!(level, 2);
                assert_eq!(message, "debug");
            }
            _ => panic!("expected debug event"),
        }
    }

    #[test]
    fn diagnostic_event_debug() {
        let info_event = DiagnosticEvent::Info {
            flag: InfoFlag::Del,
            level: 3,
            code: LogCode::Info,
            message: "delete".to_owned(),
        };
        let debug = format!("{info_event:?}");
        assert!(debug.contains("Info"));
        assert!(debug.contains("Del"));

        let debug_event = DiagnosticEvent::Debug {
            flag: DebugFlag::Io,
            level: 4,
            code: LogCode::Info,
            message: "io debug".to_owned(),
        };
        let debug = format!("{debug_event:?}");
        assert!(debug.contains("Debug"));
        assert!(debug.contains("Io"));
    }

    #[test]
    fn apply_info_flag_valid_token() {
        init(VerbosityConfig::default());
        let result = apply_info_flag("copy2");
        assert!(result.is_ok());
        assert!(info_gte(InfoFlag::Copy, 2));
    }

    #[test]
    fn apply_info_flag_invalid_token() {
        init(VerbosityConfig::default());
        let result = apply_info_flag("invalid_flag");
        assert!(result.is_err());
    }

    #[test]
    fn apply_debug_flag_valid_token() {
        init(VerbosityConfig::default());
        let result = apply_debug_flag("io3");
        assert!(result.is_ok());
        assert!(debug_gte(DebugFlag::Io, 3));
    }

    #[test]
    fn apply_debug_flag_invalid_token() {
        init(VerbosityConfig::default());
        let result = apply_debug_flag("not_a_flag");
        assert!(result.is_err());
    }

    /// A preview pass's notices must vanish while earlier ones survive, or the
    /// real run repeats the preview's lines (or loses the ones queued before).
    #[test]
    fn discard_events_from_drops_only_the_closures_events() {
        let _ = drain_events();
        emit_info(InfoFlag::Misc, 0, "before".to_owned());
        let value = discard_events_from(|| {
            emit_info(InfoFlag::Misc, 0, "preview".to_owned());
            7
        });
        assert_eq!(value, 7);
        let kept: Vec<String> = drain_events()
            .into_iter()
            .filter_map(|event| match event {
                DiagnosticEvent::Info { message, .. } => Some(message),
                _ => None,
            })
            .collect();
        assert_eq!(kept, ["before"]);
    }

    #[test]
    fn info_gte_default_config() {
        init(VerbosityConfig::default());
        assert!(!info_gte(InfoFlag::Backup, 1));
        assert!(info_gte(InfoFlag::Backup, 0));
    }

    #[test]
    fn debug_gte_default_config() {
        init(VerbosityConfig::default());
        assert!(!debug_gte(DebugFlag::Acl, 1));
        assert!(debug_gte(DebugFlag::Acl, 0));
    }

    #[test]
    fn oc_accelerated_io_debug_flags_gated_off_by_default() {
        init(VerbosityConfig::default());
        drain_events();

        // Default config leaves oc-specific categories at level 0, so a
        // gated debug_log! must be a no-op and emit nothing.
        for flag in [
            DebugFlag::Iouring,
            DebugFlag::Clone,
            DebugFlag::Sockopt,
            DebugFlag::Iocp,
        ] {
            assert!(!debug_gte(flag, 1));
        }
        crate::debug_log!(Iouring, 1, "should not emit");
        crate::debug_log!(Clone, 1, "should not emit");
        crate::debug_log!(Sockopt, 1, "should not emit");
        crate::debug_log!(Iocp, 1, "should not emit");
        assert!(drain_events().is_empty());

        // Enabling the category makes the same call emit exactly one event.
        let mut config = VerbosityConfig::default();
        config.debug.clone = 1;
        init(config);
        crate::debug_log!(Clone, 1, "CoW clone succeeded");
        assert_eq!(drain_events().len(), 1);
    }

    #[test]
    fn multiple_events_ordering() {
        init(VerbosityConfig::default());
        drain_events();

        emit_info(InfoFlag::Copy, 1, "first".to_owned());
        emit_info(InfoFlag::Del, 2, "second".to_owned());
        emit_debug(DebugFlag::Send, 3, "third".to_owned());

        let events = drain_events();
        assert_eq!(events.len(), 3);

        match &events[0] {
            DiagnosticEvent::Info { message, .. } => assert_eq!(message, "first"),
            _ => panic!("expected info"),
        }
        match &events[1] {
            DiagnosticEvent::Info { message, .. } => assert_eq!(message, "second"),
            _ => panic!("expected info"),
        }
        match &events[2] {
            DiagnosticEvent::Debug { message, .. } => assert_eq!(message, "third"),
            _ => panic!("expected debug"),
        }
    }

    #[test]
    fn drain_events_clears_buffer() {
        init(VerbosityConfig::default());
        drain_events();

        emit_info(InfoFlag::Copy, 1, "test".to_owned());
        let first_drain = drain_events();
        assert_eq!(first_drain.len(), 1);

        let second_drain = drain_events();
        assert_eq!(second_drain.len(), 0);
    }

    #[test]
    fn reinit_overwrites_config() {
        let mut config1 = VerbosityConfig::default();
        config1.info.copy = 5;
        init(config1);
        assert!(info_gte(InfoFlag::Copy, 5));

        let config2 = VerbosityConfig::default();
        init(config2);
        assert!(!info_gte(InfoFlag::Copy, 1));
    }

    /// upstream: log.c:344-366 - a server frames FINFO (every --info/--debug
    /// line) to the peer, so the peer drain takes it along with the error
    /// class, in emission order.
    #[test]
    fn peer_drain_takes_finfo_and_errors_in_order() {
        init(VerbosityConfig::default());
        set_quiet(false);
        let _ = drain_events();
        emit_debug(DebugFlag::Recv, 1, "debug line".to_owned());
        emit_warning("warn line".to_owned());
        emit_info_coded(InfoFlag::Misc, 0, LogCode::Log, "log only".to_owned());
        let texts: Vec<String> = drain_events_for_peer()
            .into_iter()
            .map(|event| match event {
                DiagnosticEvent::Info { message, .. } | DiagnosticEvent::Debug { message, .. } => {
                    message
                }
                DiagnosticEvent::Bytes { message, .. } => {
                    String::from_utf8_lossy(&message).into_owned()
                }
            })
            .collect();
        assert_eq!(texts, ["debug line", "warn line"]);
        assert_eq!(
            drain_events_coded(LogCode::Log).len(),
            1,
            "FLOG stays for the log drain"
        );
    }

    /// upstream: log.c:344-346 - `case FINFO: if (quiet) return;` runs before
    /// the send, so a quiet server frames no FINFO, yet still frames errors.
    #[test]
    fn quiet_peer_drain_discards_finfo_but_keeps_errors() {
        init(VerbosityConfig::default());
        let _ = drain_events();
        set_quiet(true);
        emit_debug(DebugFlag::Recv, 1, "debug line".to_owned());
        emit_warning("warn line".to_owned());
        let events = drain_events_for_peer();
        set_quiet(false);
        assert_eq!(events.len(), 1, "{events:?}");
        assert!(
            drain_events().is_empty(),
            "suppressed FINFO must not linger"
        );
    }

    /// A client receiver keeps its FINFO for its own renderer and drains
    /// only the error class.
    #[test]
    fn error_drain_leaves_finfo_queued() {
        init(VerbosityConfig::default());
        let _ = drain_events();
        emit_debug(DebugFlag::Recv, 1, "debug line".to_owned());
        emit_warning("warn line".to_owned());
        assert_eq!(drain_error_events().len(), 1);
        assert_eq!(drain_events().len(), 1, "the debug line is still queued");
    }

    /// upstream: options.c:546-572 - explicit levels above the cap's rows are
    /// lowered per flag; levels already below stay; a level above
    /// MAX_VERBOSITY applies no limit (options.c:551-552).
    #[test]
    fn limit_output_verbosity_caps_each_flag() {
        init(VerbosityConfig::from_verbose_level(1));
        apply_debug_flag("flist2").unwrap();
        apply_debug_flag("recv3").unwrap();
        limit_output_verbosity(3);
        assert!(debug_gte(DebugFlag::Flist, 2), "row 3 holds FLIST2");
        assert!(debug_gte(DebugFlag::Recv, 1) && !debug_gte(DebugFlag::Recv, 2));
        limit_output_verbosity(1);
        assert!(!debug_gte(DebugFlag::Flist, 1) && !debug_gte(DebugFlag::Recv, 1));
        assert!(
            info_gte(InfoFlag::Name, 1),
            "row 1 info survives a cap of 1"
        );

        init(VerbosityConfig::default());
        apply_debug_flag("flist4").unwrap();
        limit_output_verbosity(crate::MAX_VERBOSITY + 1);
        assert!(debug_gte(DebugFlag::Flist, 4));
        init(VerbosityConfig::default());
    }
}

#[cfg(test)]
mod sink_tests {
    use super::*;

    /// Records what a live sink receives, in the order it receives it.
    #[derive(Default)]
    struct Recorder(Mutex<Vec<String>>);

    impl DiagnosticSink for Recorder {
        fn write(&self, event: &Stamped<DiagnosticEvent>) {
            let text = match event.value() {
                DiagnosticEvent::Info { message, .. } | DiagnosticEvent::Debug { message, .. } => {
                    message.clone()
                }
                DiagnosticEvent::Bytes { message, .. } => {
                    String::from_utf8_lossy(message).into_owned()
                }
            };
            self.0.lock().unwrap().push(text);
        }
    }

    impl Recorder {
        fn lines(&self) -> Vec<String> {
            self.0.lock().unwrap().clone()
        }
    }

    /// upstream: log.c:272-373 - rwrite() writes each message as it is
    /// produced, so a sink must see buffered lines first and later ones at once.
    #[test]
    fn an_installed_sink_gets_the_backlog_then_each_line_as_produced() {
        let context = LogContext::new();
        let _guard = enter(Arc::clone(&context));
        emit_info(InfoFlag::Copy, 1, "before".to_owned());
        let recorder = Arc::new(Recorder::default());
        context.install_sink(recorder.clone());
        assert_eq!(recorder.lines(), ["before"]);
        emit_debug(DebugFlag::Recv, 1, "after".to_owned());
        assert_eq!(recorder.lines(), ["before", "after"]);
        assert!(drain_events().is_empty(), "a live sink leaves nothing buffered");
    }

    /// The disk-commit and rayon threads must log as the main thread does:
    /// upstream's levels are process globals (options.c:263).
    #[test]
    fn a_spawned_thread_sees_the_session_levels_and_sink() {
        let context = LogContext::new();
        let _guard = enter(Arc::clone(&context));
        let mut config = VerbosityConfig::default();
        config.debug.recv = 2;
        init(config);
        let recorder = Arc::new(Recorder::default());
        context.install_sink(recorder.clone());
        let inherited = current();
        std::thread::spawn(move || {
            let _guard = enter(inherited);
            assert!(debug_gte(DebugFlag::Recv, 2));
            emit_debug(DebugFlag::Recv, 2, "from worker".to_owned());
        })
        .join()
        .unwrap();
        assert_eq!(recorder.lines(), ["from worker"]);
    }

    /// A thread that entered nothing uses the process default, so a
    /// single-session process needs no explicit propagation.
    #[test]
    fn a_thread_without_a_context_uses_the_process_default() {
        let mut config = VerbosityConfig::default();
        config.info.copy = 3;
        init(config);
        let seen = std::thread::spawn(|| info_gte(InfoFlag::Copy, 3)).join().unwrap();
        assert!(seen);
    }

    /// Two QUIC sessions in one process must not share levels or output.
    #[test]
    fn entered_contexts_are_isolated() {
        let first = LogContext::new();
        let second = LogContext::new();
        {
            let _guard = enter(Arc::clone(&first));
            let mut config = VerbosityConfig::default();
            config.debug.send = 1;
            init(config);
            emit_debug(DebugFlag::Send, 1, "first".to_owned());
        }
        let _guard = enter(Arc::clone(&second));
        assert!(!debug_gte(DebugFlag::Send, 1));
        assert!(drain_events().is_empty());
        drop(_guard);
        let _guard = enter(first);
        assert_eq!(drain_events().len(), 1);
    }

    #[test]
    fn discard_drops_lines_bound_for_a_live_sink() {
        let context = LogContext::new();
        let _guard = enter(Arc::clone(&context));
        let recorder = Arc::new(Recorder::default());
        context.install_sink(recorder.clone());
        discard_events_from(|| emit_info(InfoFlag::Copy, 1, "preview".to_owned()));
        emit_info(InfoFlag::Copy, 1, "real".to_owned());
        assert_eq!(recorder.lines(), ["real"]);
    }

    #[test]
    fn a_level_change_reaches_a_thread_that_cached_the_old_one() {
        let context = LogContext::new();
        let _guard = enter(Arc::clone(&context));
        assert!(!debug_gte(DebugFlag::Genr, 1));
        apply_debug_flag("genr").unwrap();
        assert!(debug_gte(DebugFlag::Genr, 1));
    }
}
