//! The single place a message's destination stream is chosen.
//!
//! Upstream funnels every diagnostic message through `rprintf` (log.c:406) or
//! `rsyserr` (log.c:453) into `rwrite` (log.c:251), which is the sole function
//! that picks a `FILE *`. 606 `rprintf(F*)` call sites, one chooser. The only
//! direct `fprintf(stderr, ...)` calls left in the message path are five sites
//! that are either pre-`log_init` (clientserver.c:1520,1563) or reporting that
//! the message machinery itself is broken (log.c's own bad-logcode arm,
//! io.c:596,614 - both immediately `exit_cleanup`).
//!
//! oc had the rule written down in nine doc comments on [`LogCode`] and
//! implemented in exactly one leaf crate. This module is the implementation.

use crate::LogCode;

/// Upstream's tri-state `msgs2stderr` (options.c:98, default 2).
///
/// Modelled as three values rather than a bool because `rwrite` keys on `== 1`
/// (log.c:253) while the server-forwarding gate keys on all three
/// (log.c:328 `am_server && msgs2stderr != 1 && (msgs2stderr != 2 || f != stderr)`).
/// A bool would collapse `Never` and `Default`, which are distinguishable there.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Msgs2Stderr {
    /// `--no-msgs2stderr` (upstream value 0).
    Never,
    /// `--msgs2stderr` (upstream value 1): every message goes to stderr.
    Always,
    /// Neither flag given (upstream value 2).
    #[default]
    Default,
}

/// Everything outside the log code that `rwrite` consults to pick a stream.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StreamContext {
    /// `--quiet` (upstream `quiet`). Suppresses FINFO inside `rwrite`
    /// (log.c:318-319) - NOT by zeroing the verbosity counter.
    pub quiet: bool,
    /// Upstream `msgs2stderr`.
    pub msgs2stderr: Msgs2Stderr,
    /// True when a daemon log or `--log-file` is active (upstream
    /// `am_daemon || logfile_name`, log.c:290).
    pub log_destination: bool,
}

/// Where a message ends up.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageStream {
    /// The client's stdout (log.c:253 default, kept by FINFO at log.c:317-320).
    Stdout,
    /// The client's stderr (log.c:310-316).
    Stderr,
    /// Written to the log destination only, never to a client stream
    /// (log.c:290-307). FLOG with no log destination is discarded.
    LogOnly,
    /// Suppressed entirely: FINFO under `--quiet` (log.c:318-319).
    Suppressed,
}

/// A log code `rwrite` refuses. Upstream prints
/// `Bad logcode in rwrite(): %d [%s]` and `exit_cleanup(RERR_MESSAGEIO)`
/// (log.c:325-327).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BadLogCode(pub LogCode);

/// Choose the destination stream for `code`, mirroring `rwrite`.
///
/// The order of the arms is upstream's order, because it is load-bearing: the
/// normalisations (FERROR_SOCKET, FERROR_UTF8, FCLIENT) happen *before* the
/// switch, so those codes never reach the switch under their own name.
///
/// # Errors
///
/// Returns [`BadLogCode`] for a code `rwrite` rejects. Callers that cannot
/// abort should surface it; they must not silently pick a stream, which is the
/// defect this function exists to prevent.
// upstream: log.c:251-328 rwrite()
pub const fn message_stream(
    code: LogCode,
    ctx: StreamContext,
) -> Result<MessageStream, BadLogCode> {
    // upstream: log.c:281-286 - FERROR_SOCKET simplifies to FERROR, and
    // FERROR_UTF8 becomes FERROR with is_utf8 forced on. Both therefore take
    // the stderr arm below rather than reaching the default.
    // upstream: log.c:288-289 - FCLIENT is rewritten to FINFO.
    let code = match code {
        LogCode::ErrorSocket | LogCode::ErrorUtf8 => LogCode::Error,
        LogCode::Client => LogCode::Info,
        other => other,
    };

    match code {
        // upstream: log.c:290-307 - an FLOG message is written to the log and
        // returns before the stream switch; with no log destination it is
        // discarded outright (log.c:306-307).
        LogCode::Log => Ok(MessageStream::LogOnly),
        // upstream: log.c:309-316 - FERROR_XFER falls through to FERROR and
        // FWARNING; all three set `f = stderr`. FERROR_XFER additionally sets
        // `got_xfer_error` (log.c:311), which is the caller's concern.
        LogCode::ErrorXfer | LogCode::Error | LogCode::Warning => Ok(MessageStream::Stderr),
        // upstream: log.c:317-320 - FINFO keeps the default stream, and
        // `quiet` returns without writing at all.
        LogCode::Info => {
            if ctx.quiet {
                Ok(MessageStream::Suppressed)
            } else {
                // upstream: log.c:253 - the default `f` is stderr only when
                // msgs2stderr == 1, so --no-msgs2stderr and the unset default
                // both keep stdout.
                match ctx.msgs2stderr {
                    Msgs2Stderr::Always => Ok(MessageStream::Stderr),
                    Msgs2Stderr::Never | Msgs2Stderr::Default => Ok(MessageStream::Stdout),
                }
            }
        }
        // upstream: log.c:325-327 - FNONE is "never sent" (rsync.h:276) and
        // lands in the default arm, which exits RERR_MESSAGEIO.
        LogCode::None => Err(BadLogCode(LogCode::None)),
        // Normalised above; listed so a code added later must be classified
        // here rather than inheriting a stream by accident.
        LogCode::ErrorSocket | LogCode::ErrorUtf8 | LogCode::Client => {
            Err(BadLogCode(LogCode::None))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BadLogCode, MessageStream, Msgs2Stderr, StreamContext, message_stream};
    use crate::LogCode;

    /// Every context `rwrite` can be called in. The class assertions below
    /// quantify over this so a routing rule that only holds for the default
    /// configuration cannot pass.
    fn all_contexts() -> Vec<StreamContext> {
        let mut out = Vec::new();
        for quiet in [false, true] {
            for msgs2stderr in [
                Msgs2Stderr::Never,
                Msgs2Stderr::Always,
                Msgs2Stderr::Default,
            ] {
                for log_destination in [false, true] {
                    out.push(StreamContext {
                        quiet,
                        msgs2stderr,
                        log_destination,
                    });
                }
            }
        }
        out
    }

    /// The codes upstream routes to stderr, taken from `rwrite`'s own
    /// structure rather than from this module: FERROR_SOCKET and FERROR_UTF8
    /// are rewritten to FERROR before the switch (log.c:281-286), and
    /// FERROR_XFER falls through to FERROR and FWARNING (log.c:309-316).
    ///
    /// Listing them independently is what makes the class assertion an oracle
    /// instead of a restatement of the implementation.
    const WARNING_CLASS: &[LogCode] = &[
        LogCode::ErrorXfer,
        LogCode::Error,
        LogCode::Warning,
        LogCode::ErrorSocket,
        LogCode::ErrorUtf8,
    ];

    /// THE CLASS GATE. #352/#7236 and #357/#7242 were the same defect found
    /// twice in independent code paths: warning-class output reaching stdout.
    /// Now there is one function that can commit it, and this fails if it does
    /// - in any context, for any warning-class code.
    #[test]
    fn no_warning_class_code_ever_reaches_stdout() {
        for &code in WARNING_CLASS {
            for ctx in all_contexts() {
                let stream = message_stream(code, ctx).unwrap_or_else(|_| {
                    panic!("{code:?} is a real logcode and must not be rejected")
                });
                assert_eq!(
                    stream,
                    MessageStream::Stderr,
                    "{code:?} must go to stderr, got {stream:?} with {ctx:?}",
                );
            }
        }
    }

    /// upstream: log.c:290-307 - FLOG is written to the log destination and
    /// returns before the stream switch, so it can never surface on a client
    /// stream regardless of configuration.
    #[test]
    fn flog_never_reaches_a_client_stream() {
        for ctx in all_contexts() {
            assert_eq!(
                message_stream(LogCode::Log, ctx),
                Ok(MessageStream::LogOnly),
                "FLOG leaked to a client stream with {ctx:?}",
            );
        }
    }

    /// upstream: log.c:325-327 - an unroutable code is a fatal error
    /// (`exit_cleanup(RERR_MESSAGEIO)`), never a silent stdout default. A
    /// mapper that guessed here is how a new code would inherit stdout.
    #[test]
    fn unroutable_code_is_an_error_not_a_default_stream() {
        for ctx in all_contexts() {
            assert_eq!(
                message_stream(LogCode::None, ctx),
                Err(BadLogCode(LogCode::None)),
                "FNONE must be rejected, not routed, with {ctx:?}",
            );
        }
    }

    /// upstream: log.c:288-289 - `if (code == FCLIENT) code = FINFO;`. FCLIENT
    /// must behave exactly as FINFO, including under --quiet and
    /// --msgs2stderr, or the normalisation is only half applied.
    #[test]
    fn fclient_is_indistinguishable_from_finfo() {
        for ctx in all_contexts() {
            assert_eq!(
                message_stream(LogCode::Client, ctx),
                message_stream(LogCode::Info, ctx),
                "FCLIENT diverged from FINFO with {ctx:?}",
            );
        }
    }

    /// upstream: log.c:318-319 - `case FINFO: if (quiet) return;`. Upstream
    /// suppresses FINFO inside rwrite; oc historically folded --quiet into
    /// `verbosity = 0` at parse time
    /// (cli/frontend/arguments/parser/entry.rs), which silences only
    /// verbosity-gated output. Pinning it here gives the rule one home.
    #[test]
    fn quiet_suppresses_info_and_nothing_else() {
        let quiet = StreamContext {
            quiet: true,
            ..StreamContext::default()
        };
        assert_eq!(
            message_stream(LogCode::Info, quiet),
            Ok(MessageStream::Suppressed)
        );
        // --quiet must not silence errors or warnings.
        for &code in WARNING_CLASS {
            assert_eq!(
                message_stream(code, quiet),
                Ok(MessageStream::Stderr),
                "--quiet must not suppress {code:?}",
            );
        }
    }

    /// upstream: log.c:253 - `FILE *f = msgs2stderr == 1 ? stderr : stdout;`.
    /// Only the `== 1` state moves the default; modelling msgs2stderr as a
    /// bool would collapse Never and Default, which differ at log.c:328.
    #[test]
    fn only_msgs2stderr_always_moves_info_to_stderr() {
        let ctx = |m| StreamContext {
            msgs2stderr: m,
            ..StreamContext::default()
        };
        assert_eq!(
            message_stream(LogCode::Info, ctx(Msgs2Stderr::Always)),
            Ok(MessageStream::Stderr)
        );
        assert_eq!(
            message_stream(LogCode::Info, ctx(Msgs2Stderr::Never)),
            Ok(MessageStream::Stdout)
        );
        assert_eq!(
            message_stream(LogCode::Info, ctx(Msgs2Stderr::Default)),
            Ok(MessageStream::Stdout)
        );
    }

    /// Total by construction: every code in the vocabulary resolves to a
    /// decision. A code added later without a home here fails to compile in
    /// `message_stream`'s exhaustive match, and fails this test if it somehow
    /// reaches a default.
    #[test]
    fn every_logcode_is_classified() {
        for &code in LogCode::all() {
            let decided = all_contexts()
                .into_iter()
                .all(|ctx| message_stream(code, ctx).is_ok() || code == LogCode::None);
            assert!(decided, "{code:?} has no defined routing");
        }
    }
}
