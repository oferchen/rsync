//! Upstream's diagnostic for a file that failed whole-file verification.
//!
//! `receiver.c:1071-1091` is one rule with five decisions in it - severity,
//! emission gate, `keptstr`, retry suffix and the format string - and every
//! decision reads state that oc keeps in different places on its two receive
//! paths. Reproducing the rule beside each path is how the wordings drift, so
//! it lives here once, in the crate that already owns [`LogCode`] and the
//! `INFO_GTE` lookup, and each path supplies its own state.

use std::path::Path;

use crate::levels::InfoFlag;
use crate::log_code::LogCode;
use crate::thread_local::info_gte;

/// The state upstream's `case 0:` reads when a file fails verification.
///
/// Every field is one upstream variable, named for it. Upstream reads them as
/// globals; oc derives them per receive path, so they are collected here to
/// keep the rule itself free of either path's data model.
///
/// [`Default`] is a plain, non-batch phase-1 run with no partial retention, no
/// in-place write and no `%i` - the state in which upstream stays silent.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct VerifyFailure {
    /// The phase-2 redo is running. Upstream's `redoing`: it promotes the
    /// message from `FWARNING` to `FERROR_XFER`, makes it unconditional, and
    /// drops the retry suffix - there is no retry left to promise.
    ///
    /// upstream: receiver.c:1071 - `msgtype = redoing ? FERROR_XFER : FWARNING`.
    pub redoing: bool,
    /// The receiver is replaying a recorded batch (`--read-batch`). Upstream's
    /// `read_batch`: a replay may only *try* the redo, because the recorded
    /// stream need not carry it.
    ///
    /// upstream: receiver.c:1085 - `redostr = read_batch ? " (may try again)"`.
    pub read_batch: bool,
    /// The per-file output format carries `%i`. Upstream's
    /// `stdout_format_has_i`, the third disjunct of the emission gate.
    ///
    /// Derive this from the resolved FORMAT, never from an `-i` boolean.
    /// `options.c:2345-2358` feeds one variable from two sources and `-i`
    /// rewrites `stdout_format` to `"%i %n%L"`, so a format-derived value
    /// catches both `-i` and a bare `--out-format='%i%n'`; an `-i` boolean
    /// misses the latter.
    ///
    /// # Narrower than upstream, deliberately
    ///
    /// Upstream's `stdout_format_has_i` is a TRI-STATE, not a flag: `2` when
    /// `am_server` and the format carries `%I`, and `itemize_changes` is a
    /// counter (`options.c:1581`), so `-ii` also yields `2`. Several upstream
    /// sites test `> 1` specifically (`generator.c:583,1010,1138`,
    /// `hlink.c:400`, `log.c:832`). This gate is not one of them -
    /// `receiver.c:1072` tests plain truthiness - so a `bool` is exact here.
    /// A caller that needs the `-i`/`-ii` distinction must carry the level
    /// itself rather than widen this field.
    ///
    /// upstream: receiver.c:1072 - note it reads `stdout_format_has_i`
    /// unconditionally, unlike `receiver.c:644`, which picks
    /// `logfile_format_has_i` instead when `am_server`.
    pub stdout_format_has_i: bool,
    /// `--partial` is in force. Upstream's `keep_partial`, which `--inplace`
    /// clears (`options.c:2439`) even though the partial file is still kept -
    /// the `inplace` disjunct below is what covers that case.
    ///
    /// upstream: receiver.c:1074.
    pub keep_partial: bool,
    /// This file has a partial path to retain, i.e. upstream's
    /// `partialptr != NULL`.
    ///
    /// upstream: receiver.c:1074.
    pub has_partial_path: bool,
    /// `--partial-dir` is in force, i.e. upstream's `partial_dir != NULL`.
    ///
    /// upstream: receiver.c:1076.
    pub partial_dir: bool,
    /// The update was written straight to the destination. Upstream's
    /// `inplace`, which `options.c:2410` also sets for `--append`, so an
    /// append that fails verification reports its update as retained rather
    /// than discarded.
    ///
    /// upstream: receiver.c:1074.
    pub inplace: bool,
}

impl VerifyFailure {
    /// Upstream's `keptstr`, describing what happened to the failed update.
    ///
    /// The chain order is load-bearing and reproduced verbatim: `inplace`
    /// falsifies the first clause, which is what lets a `--partial-dir` run
    /// reach the second rather than reporting the update as discarded.
    ///
    /// upstream: receiver.c:1074-1079.
    const fn kept_str(self) -> &'static str {
        // upstream's first clause, `!(keep_partial && partialptr) && !inplace`,
        // negated once so the two ways an update survives read positively.
        let survives = self.inplace || (self.keep_partial && self.has_partial_path);
        if !survives {
            "discarded"
        } else if self.partial_dir {
            "put into partial-dir"
        } else {
            "retained"
        }
    }

    /// Whether upstream prints the line at all.
    ///
    /// The `FERROR_XFER` form short-circuits the gate, so a phase-2 failure is
    /// always reported; the `FWARNING` form needs per-file output to have been
    /// asked for, through the `NAME` info category (`-v`, `--info=name`) or a
    /// format carrying `%i`. A plain `-a` run is therefore silent about a
    /// failure it is about to retry successfully.
    ///
    /// The `NAME` level comes from the thread-local verbosity configuration,
    /// seeded on a client from its own command line and on a server receiver
    /// from the flags the client forwarded, so `--info=name0` suppresses the
    /// line even under `-v`, exactly as upstream's `INFO_GTE` does.
    ///
    /// upstream: receiver.c:1072 - `if (msgtype == FERROR_XFER ||
    /// INFO_GTE(NAME, 1) || stdout_format_has_i)`.
    fn is_reported(self) -> bool {
        self.redoing || info_gte(InfoFlag::Name, 1) || self.stdout_format_has_i
    }
}

/// Renders upstream's verification-failure line, or `None` when upstream stays
/// silent.
///
/// `name` is the file's *file list* name. Upstream's receiver has already
/// `change_dir()`ed into the destination root (`main.c:815`), so `fname` - and
/// `f_name(file, NULL)` in the `local_name` case - renders relative to it; a
/// joined absolute destination path is never what a user sees here.
///
/// The returned [`LogCode`] is upstream's `msgtype`. Callers route it: the
/// network receiver converts it to a multiplexed `MessageCode`, the local
/// executor writes it to the stream `log.c:313-316` routes that code to.
///
/// # Upstream Reference
///
/// - `receiver.c:1071` - `msgtype = redoing ? FERROR_XFER : FWARNING`.
/// - `receiver.c:1072` - the emission gate, [`VerifyFailure::is_reported`].
/// - `receiver.c:1073-1079` - `keptstr`, [`VerifyFailure::kept_str`].
/// - `receiver.c:1080-1087` - `errstr` and `redostr`.
/// - `receiver.c:1088-1091` - the format string reproduced below.
#[must_use]
pub fn verification_failure(name: &Path, state: VerifyFailure) -> Option<(LogCode, String)> {
    if !state.is_reported() {
        return None;
    }
    let kept = state.kept_str();
    let name = name.display();
    if state.redoing {
        return Some((
            LogCode::ErrorXfer,
            format!("ERROR: {name} failed verification -- update {kept}."),
        ));
    }
    let redostr = if state.read_batch {
        " (may try again)"
    } else {
        " (will try again)"
    };
    Some((
        LogCode::Warning,
        format!("WARNING: {name} failed verification -- update {kept}{redostr}."),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::VerbosityConfig;
    use crate::thread_local::init;

    /// Sets the thread-local `NAME` level the emission gate reads.
    fn set_name_level(reported: bool) {
        init(VerbosityConfig::from_verbose_level(u8::from(reported)));
    }

    fn line(state: VerifyFailure) -> Option<(LogCode, String)> {
        verification_failure(Path::new("sub/f.txt"), state)
    }

    /// Every `keptstr` branch, driven off the four inputs upstream tests.
    ///
    /// Rows 3 and 7 are unreachable from the command line - upstream sets
    /// `keep_partial` whenever `--partial-dir` is given, and rejects
    /// `--inplace --partial-dir` outright (`options.c:2426-2431`) - but they
    /// are what pin the if/else-if ORDER at receiver.c:1074-1079. Rewriting
    /// the chain as three independent predicates still passes every reachable
    /// row and fails these two.
    #[test]
    fn kept_str_covers_every_upstream_branch() {
        set_name_level(true);
        // (keep_partial, has_partial_path, partial_dir, inplace, expected)
        let table = [
            (false, false, false, false, "discarded"),
            (true, false, false, false, "discarded"),
            (false, false, true, false, "discarded"),
            (true, true, true, false, "put into partial-dir"),
            (true, true, false, false, "retained"),
            (false, false, false, true, "retained"),
            (false, false, true, true, "put into partial-dir"),
        ];
        for (keep_partial, has_partial_path, partial_dir, inplace, expected) in table {
            let state = VerifyFailure {
                keep_partial,
                has_partial_path,
                partial_dir,
                inplace,
                ..VerifyFailure::default()
            };
            let (_, warning) = line(state).expect("reported at NAME level 1");
            assert_eq!(
                warning,
                format!(
                    "WARNING: sub/f.txt failed verification -- update {expected} (will try again)."
                ),
                "keptstr for {state:?}"
            );
            let (_, error) = line(VerifyFailure {
                redoing: true,
                ..state
            })
            .expect("the FERROR_XFER form is unconditional");
            assert_eq!(
                error,
                format!("ERROR: sub/f.txt failed verification -- update {expected}."),
                "keptstr must not depend on the severity, for {state:?}"
            );
        }
    }

    /// `redoing` selects the severity, and with it the retry suffix: the
    /// phase-2 form promises nothing because no retry remains.
    ///
    /// upstream: receiver.c:1071,1080-1086.
    #[test]
    fn redoing_selects_severity_and_drops_the_retry_suffix() {
        set_name_level(true);
        for read_batch in [false, true] {
            let (code, message) = line(VerifyFailure {
                redoing: true,
                read_batch,
                ..VerifyFailure::default()
            })
            .expect("phase 2 always reports");
            assert_eq!(code, LogCode::ErrorXfer);
            assert_eq!(
                message, "ERROR: sub/f.txt failed verification -- update discarded.",
                "redostr is \"\" on the FERROR_XFER branch, batch or not"
            );
        }
    }

    /// The `FWARNING` form carries `redostr`, and `--read-batch` downgrades the
    /// promise to "may".
    ///
    /// upstream: receiver.c:1085-1086.
    #[test]
    fn read_batch_downgrades_the_retry_promise() {
        set_name_level(true);
        let (code, message) = line(VerifyFailure {
            read_batch: true,
            ..VerifyFailure::default()
        })
        .expect("reported at NAME level 1");
        assert_eq!(code, LogCode::Warning);
        assert_eq!(
            message,
            "WARNING: sub/f.txt failed verification -- update discarded (may try again)."
        );
    }

    /// The emission gate: `FERROR_XFER` short-circuits it, the warning needs
    /// per-file output from either `INFO_GTE(NAME, 1)` or `%i`.
    ///
    /// upstream: receiver.c:1072.
    #[test]
    fn the_gate_covers_all_three_disjuncts() {
        set_name_level(false);
        assert!(
            line(VerifyFailure::default()).is_none(),
            "a plain run is silent about a failure it will retry"
        );
        assert!(
            line(VerifyFailure {
                stdout_format_has_i: true,
                ..VerifyFailure::default()
            })
            .is_some(),
            "stdout_format_has_i is the third disjunct"
        );
        assert!(
            line(VerifyFailure {
                redoing: true,
                ..VerifyFailure::default()
            })
            .is_some(),
            "msgtype == FERROR_XFER short-circuits the gate"
        );

        set_name_level(true);
        assert!(
            line(VerifyFailure::default()).is_some(),
            "INFO_GTE(NAME, 1) is the second disjunct"
        );
    }

    /// The name is rendered as given, never absolutised.
    ///
    /// upstream: receiver.c:1089-1090.
    #[test]
    fn the_name_is_printed_verbatim() {
        set_name_level(true);
        let (_, message) = verification_failure(
            Path::new("deep/nested/payload.bin"),
            VerifyFailure::default(),
        )
        .expect("reported");
        assert!(
            message.starts_with("WARNING: deep/nested/payload.bin failed verification"),
            "got {message}"
        );
    }
}
