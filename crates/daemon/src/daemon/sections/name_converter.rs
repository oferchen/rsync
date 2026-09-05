/// Long-lived subprocess providing uid/gid name conversion in chroot environments.
///
/// When a daemon module specifies `name converter`, this subprocess replaces
/// NSS lookups (getpwuid, getpwnam, getgrgid, getgrnam) with a simple
/// line-based protocol over stdin/stdout pipes.
///
/// upstream: clientserver.c:964-971 - the name converter is spawned after
/// privilege reduction and communicates via stdin/stdout pipes. Requests are
/// `"{cmd} {arg}\n"`, responses are single lines.
#[cfg(unix)]
struct NameConverter {
    child: std::process::Child,
    /// The request/answer pipes, or the reason they can no longer be used.
    ///
    /// A converter that has died stays dead: the state lives in the type, so a
    /// later lookup cannot find a usable stream where there is none and read
    /// that absence as "this name has no id". Upstream reaches the same place
    /// by dying itself - a write to a converter that has gone away hits
    /// clientserver.c:1329-1334 and `exit_cleanup(RERR_SOCKETIO)`.
    stream: Result<ConverterStream, ConverterDeath>,
}

/// The two halves of the converter's line protocol.
#[cfg(unix)]
struct ConverterStream {
    stdin: io::BufWriter<std::process::ChildStdin>,
    stdout: io::BufReader<std::process::ChildStdout>,
}

/// Why the converter can no longer be spoken to.
///
/// Kept as the parts of an `io::Error` rather than the error itself, because
/// every later lookup has to be told the same thing and `io::Error` is not
/// `Clone`.
#[cfg(unix)]
struct ConverterDeath {
    kind: io::ErrorKind,
    detail: String,
}

#[cfg(unix)]
impl ConverterDeath {
    /// Records a failed pipe operation.
    fn from_io(context: &str, err: &io::Error) -> Self {
        Self {
            kind: err.kind(),
            detail: format!("name converter: {context}: {err}"),
        }
    }

    /// Records the converter having closed its answer pipe.
    ///
    /// upstream: clientserver.c:1336-1337 - `read_line_old()` fails for this
    /// call, and the next request's write then exits the process at :1333. The
    /// session ends either way; taking it here is what keeps the *first* such
    /// lookup from reading as "no such name".
    fn exited() -> Self {
        Self {
            kind: io::ErrorKind::UnexpectedEof,
            detail: "name converter: closed its answer pipe".to_owned(),
        }
    }

    /// Reproduces the error to hand to this lookup's caller.
    fn error(&self) -> io::Error {
        io::Error::new(self.kind, self.detail.clone())
    }
}

#[cfg(unix)]
impl ConverterStream {
    /// Writes one request line and reads one answer line.
    ///
    /// `Ok(None)` is an answer that names nothing (upstream's empty line);
    /// `Err` means the stream is broken and the converter is finished.
    fn exchange(&mut self, request: &str) -> Result<Option<String>, ConverterDeath> {
        // upstream: clientserver.c:1329-1334 - a write that does not complete
        // is `exit_cleanup(RERR_SOCKETIO)`, never a lookup that "found
        // nothing".
        self.stdin
            .write_all(request.as_bytes())
            .map_err(|err| ConverterDeath::from_io("writing the request", &err))?;
        self.stdin
            .flush()
            .map_err(|err| ConverterDeath::from_io("writing the request", &err))?;

        let mut line = String::new();
        match self.stdout.read_line(&mut line) {
            // upstream: clientserver.c:1336-1337 - `read_line_old()` failed.
            Ok(0) => Err(ConverterDeath::exited()),
            Err(err) => Err(ConverterDeath::from_io("reading the answer", &err)),
            Ok(_) => {
                let trimmed = line.trim_end().to_owned();
                // upstream: clientserver.c:1345-1346 - `if (!*buf) return
                // False`. An empty answer is "unknown"; reading it as the id
                // `atol("") == 0` is CVE-2026-53798.
                Ok((!trimmed.is_empty()).then_some(trimmed))
            }
        }
    }
}

#[cfg(unix)]
impl NameConverter {
    /// Spawns the converter subprocess via `sh -c`.
    fn spawn(command: &str) -> io::Result<Self> {
        let mut child = ProcessCommand::new("sh")
            .args(["-c", command])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        let stdin = io::BufWriter::new(child.stdin.take().expect("stdin piped"));
        let stdout = io::BufReader::new(child.stdout.take().expect("stdout piped"));

        Ok(Self {
            child,
            stream: Ok(ConverterStream { stdin, stdout }),
        })
    }

    /// Sends a query to the converter and reads one line of response.
    ///
    /// The four outcomes are upstream's, un-merged:
    ///
    /// | outcome    | cause                                | upstream                  |
    /// |------------|--------------------------------------|---------------------------|
    /// | `Resolved` | a usable answer line                 | clientserver.c:1355 True  |
    /// | `Unknown`  | an empty answer line                 | clientserver.c:1345 False |
    /// | `Refused`  | a control byte in `arg`; nothing sent| clientserver.c:1317 False |
    /// | `Failed`   | over-long request, or broken stream  | clientserver.c:1326, 1333 |
    fn query(&mut self, cmd: &str, arg: &str) -> ConverterOutcome<String> {
        // `arg` is rejected outright when it carries a control byte: the
        // request framing is one line per query, so an embedded newline would
        // make the converter answer twice while this reads once, leaving the
        // surplus answer buffered for the *next* lookup to consume.
        if !is_safe_token(arg) {
            // Upstream reports the refusal before returning, so the operator
            // learns that a peer-supplied name was rejected rather than merely
            // unknown. Emitting at the guard - not at a caller - keeps the one
            // site that decides also the one site that speaks.
            // upstream: clientserver.c:1317-1319 `rprintf(FERROR, "invalid
            // name-converter token: %s\n", *name_p)` then `return False`.
            logging::emit_info_coded(
                logging::InfoFlag::Misc,
                0,
                logging::LogCode::Error,
                format!("invalid name-converter token: {arg}"),
            );
            return ConverterOutcome::Refused;
        }
        let request = format!("{cmd} {arg}\n");
        // upstream: clientserver.c:1324 - `len >= (int)sizeof buf` on a 1024-byte
        // buffer, so 1024 itself is already too long (snprintf truncated it).
        // Upstream answers a truncated request with
        // `exit_cleanup(RERR_UNSUPPORTED)`, so it is a failure and not a name
        // that "has no id". The stream stays usable: nothing was written.
        if request.len() >= NAMECVT_REQUEST_LIMIT {
            return ConverterOutcome::Failed(io::Error::new(
                io::ErrorKind::InvalidInput,
                "name converter: request too large to frame",
            ));
        }

        let stream = match &mut self.stream {
            Ok(stream) => stream,
            Err(death) => return ConverterOutcome::Failed(death.error()),
        };

        match stream.exchange(&request) {
            Ok(Some(answer)) => ConverterOutcome::Resolved(answer),
            Ok(None) => ConverterOutcome::Unknown,
            Err(death) => {
                let err = death.error();
                self.stream = Err(death);
                ConverterOutcome::Failed(err)
            }
        }
    }

    /// Converts a numeric UID to a username.
    fn uid_to_name(&mut self, uid: u32) -> ConverterOutcome<String> {
        self.query("uid", &uid.to_string())
    }

    /// Converts a numeric GID to a group name.
    fn gid_to_name(&mut self, gid: u32) -> ConverterOutcome<String> {
        self.query("gid", &gid.to_string())
    }

    /// Converts a username to a numeric UID.
    fn name_to_uid(&mut self, name: &str) -> ConverterOutcome<u32> {
        self.query("usr", name)
            .map_answer(|answer| parse_converter_id(&answer))
    }

    /// Converts a group name to a numeric GID.
    fn name_to_gid(&mut self, name: &str) -> ConverterOutcome<u32> {
        self.query("grp", name)
            .map_answer(|answer| parse_converter_id(&answer))
    }
}

/// Upper bound on a converter request, matching upstream's `char buf[1024]`.
///
/// upstream: clientserver.c:1313 - `char buf[1024]`, refused at `:1324` once the
/// formatted request reaches that length.
#[cfg(unix)]
const NAMECVT_REQUEST_LIMIT: usize = 1024;

/// Rejects a name that cannot be framed as a single request line.
///
/// upstream: clientserver.c:1362-1370 `namecvt_safe_token()` - refuses any byte
/// below `' '` or equal to `0x7f`, checked at `:1317` *before* the request is
/// formatted. Newline is the byte that matters (it desynchronises the
/// request/answer stream); the rest of the control range comes along because
/// upstream refuses the whole class rather than one byte.
#[cfg(unix)]
fn is_safe_token(name: &str) -> bool {
    !name.bytes().any(|byte| byte < b' ' || byte == 0x7f)
}

/// Parses a converter's id answer under upstream's strict digit rule.
///
/// upstream: clientserver.c:1345-1354 - an empty line, any non-digit byte, or a
/// value that does not fit `id_t` yields `False` and leaves the id untouched.
/// The explicit digit loop is why a leading `+` is refused, which Rust's
/// `str::parse::<u32>` would otherwise accept.
#[cfg(unix)]
fn parse_converter_id(answer: &str) -> Option<u32> {
    if answer.is_empty() || !answer.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    answer.parse().ok()
}

#[cfg(unix)]
impl Drop for NameConverter {
    fn drop(&mut self) {
        // Send SIGKILL and reap to prevent zombie processes.
        // The stdin pipe is closed when BufWriter is dropped, signalling EOF.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(unix)]
impl metadata::id_lookup::NameConverterCallbacks for NameConverter {
    fn uid_to_name(&mut self, uid: u32) -> ConverterOutcome<String> {
        self.uid_to_name(uid)
    }
    fn gid_to_name(&mut self, gid: u32) -> ConverterOutcome<String> {
        self.gid_to_name(gid)
    }
    fn name_to_uid(&mut self, name: &str) -> ConverterOutcome<u32> {
        self.name_to_uid(name)
    }
    fn name_to_gid(&mut self, name: &str) -> ConverterOutcome<u32> {
        self.name_to_gid(name)
    }
}

/// Windows name converter using Win32 account APIs directly.
///
/// Unlike the Unix subprocess-based converter, Windows name resolution
/// uses `LookupAccountNameW` and `NetUserEnum` from the platform crate.
/// No subprocess is needed since Windows doesn't use chroot.
///
/// upstream: uidlist.c - on Windows, name resolution uses Win32 APIs
/// instead of NSS/getpwuid/getgrnam.
#[cfg(windows)]
struct WindowsNameConverter;

#[cfg(windows)]
impl WindowsNameConverter {
    /// Creates a new Windows name converter.
    fn new() -> Self {
        Self
    }
}

#[cfg(windows)]
impl metadata::id_lookup::NameConverterCallbacks for WindowsNameConverter {
    fn uid_to_name(&mut self, uid: u32) -> ConverterOutcome<String> {
        platform::name_resolution::rid_to_account_name(uid).into()
    }
    fn gid_to_name(&mut self, gid: u32) -> ConverterOutcome<String> {
        // On Windows, GIDs map to group RIDs. Resolve them through the same
        // rid_to_account_name path used for user RIDs, which matches accounts
        // by RID regardless of whether they name a user or a local group.
        platform::name_resolution::rid_to_account_name(gid).into()
    }
    fn name_to_uid(&mut self, name: &str) -> ConverterOutcome<u32> {
        platform::name_resolution::name_to_rid(name).into()
    }
    fn name_to_gid(&mut self, name: &str) -> ConverterOutcome<u32> {
        platform::name_resolution::lookup_account_info(name)
            .map(|(rid, _)| rid)
            .into()
    }
}

/// RAII guard that removes the thread-local name converter on drop.
///
/// Ensures the converter is cleaned up even on early return or panic,
/// preventing stale converters from leaking across transfers.
struct NameConverterGuard;

impl Drop for NameConverterGuard {
    fn drop(&mut self) {
        metadata::id_lookup::clear_name_converter();
    }
}

/// Installs a name converter into the current thread's lookup slot.
///
/// Returns an RAII guard that removes the converter on drop.
#[cfg(unix)]
fn install_name_converter(converter: NameConverter) -> NameConverterGuard {
    metadata::id_lookup::set_name_converter(Box::new(converter));
    NameConverterGuard
}

/// Installs a Windows name converter into the current thread's lookup slot.
///
/// Returns an RAII guard that removes the converter on drop.
#[cfg(windows)]
fn install_windows_name_converter() -> NameConverterGuard {
    metadata::id_lookup::set_name_converter(Box::new(WindowsNameConverter::new()));
    NameConverterGuard
}

#[cfg(all(test, unix))]
mod name_converter_tests {
    use super::*;

    /// The answered value, with the three non-answers kept apart.
    ///
    /// A `Failed` outcome panics instead of reading as `None`: collapsing it
    /// into "this name has no id" is the defect the outcome type exists to
    /// prevent, and a helper that quietly did so would hide it from every
    /// test below.
    #[cfg(unix)]
    fn answered<T: std::fmt::Debug>(outcome: ConverterOutcome<T>) -> Option<T> {
        match outcome {
            ConverterOutcome::Resolved(value) => Some(value),
            ConverterOutcome::Unknown | ConverterOutcome::Refused => None,
            ConverterOutcome::Failed(err) => panic!("the converter failed: {err}"),
        }
    }

    #[cfg(unix)]
    fn is_failure<T>(outcome: &ConverterOutcome<T>) -> bool {
        matches!(outcome, ConverterOutcome::Failed(_))
    }

    #[cfg(unix)]
    #[test]
    fn spawn_and_query_uid_to_name() {
        // Simple script that echoes a fixed name for any uid query
        let mut nc = NameConverter::spawn("while read cmd arg; do echo testuser; done")
            .expect("spawn should succeed");
        assert_eq!(answered(nc.uid_to_name(1000)), Some("testuser".to_owned()));
        assert_eq!(answered(nc.uid_to_name(0)), Some("testuser".to_owned()));
    }

    #[cfg(unix)]
    #[test]
    fn spawn_and_query_gid_to_name() {
        let mut nc = NameConverter::spawn("while read cmd arg; do echo testgroup; done")
            .expect("spawn should succeed");
        assert_eq!(answered(nc.gid_to_name(1000)), Some("testgroup".to_owned()));
    }

    #[cfg(unix)]
    #[test]
    fn spawn_and_query_name_to_uid() {
        let mut nc = NameConverter::spawn("while read cmd arg; do echo 1001; done")
            .expect("spawn should succeed");
        assert_eq!(answered(nc.name_to_uid("alice")), Some(1001));
    }

    #[cfg(unix)]
    #[test]
    fn spawn_and_query_name_to_gid() {
        let mut nc = NameConverter::spawn("while read cmd arg; do echo 2002; done")
            .expect("spawn should succeed");
        assert_eq!(answered(nc.name_to_gid("staff")), Some(2002));
    }

    /// upstream: clientserver.c:1345-1346 - `if (!*buf) return False`. An empty
    /// answer is the converter saying "I do not know", which is NOT a failure:
    /// the converter must still be usable afterwards.
    #[cfg(unix)]
    #[test]
    fn an_empty_answer_is_unknown_and_leaves_the_converter_usable() {
        let mut nc = NameConverter::spawn(
            r#"while read cmd arg; do
                case "$arg" in
                    alice) echo 1001 ;;
                    *)     echo '' ;;
                esac
            done"#,
        )
        .expect("spawn should succeed");

        let outcome = nc.name_to_uid("nobody");
        assert!(
            matches!(outcome, ConverterOutcome::Unknown),
            "an empty answer is `unknown`, not a failure: {outcome:?}"
        );
        assert_eq!(
            answered(nc.name_to_uid("alice")),
            Some(1001),
            "an `unknown` answer must not end the converter's usefulness"
        );
    }

    /// THE FAIL-OPEN CASE. A converter that has died must not read as "this
    /// name has no id" - that answer would hand the caller the sender's own
    /// numeric id for every name for the rest of the session.
    ///
    /// upstream: clientserver.c:1336-1337 returns False for the EOF itself and
    /// :1329-1334 then exits the process on the NEXT request's write, so the
    /// session never continues past a dead converter either.
    #[cfg(unix)]
    #[test]
    fn a_dead_converter_fails_instead_of_answering_unknown() {
        // The child exits immediately: the answer pipe is at EOF.
        let mut nc = NameConverter::spawn("exit 0").expect("spawn should succeed");

        // Which cause is recorded is a RACE, and both arms are legitimate: if
        // the child has already exited and closed the pipe, the request WRITE
        // fails with EPIPE; if it has not, the write lands and the answer READ
        // hits EOF. Pinning one of them makes this cell fail on whichever host
        // loses the race - it pinned `UnexpectedEof` and CI measured
        // `BrokenPipe` on all three nextest retries. What the test is actually
        // for is that the cause is RECORDED and REPLAYED, so capture whichever
        // one won and assert the stickiness against it.
        let first = nc.uid_to_name(1000);
        let recorded = match &first {
            ConverterOutcome::Failed(err) => err.kind(),
            other => panic!("EOF from the converter must be a failure, not `unknown`: {other:?}"),
        };
        assert!(
            matches!(
                recorded,
                io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe
            ),
            "a dead converter must fail from the pipe, not some unrelated cause: {recorded:?}"
        );

        // Sticky: the death is recorded in the type, so every later lookup
        // reports it too. Without that, one dead converter degrades into a
        // silent "no such name" for every subsequent id in the transfer.
        for later in [
            is_failure(&nc.uid_to_name(1001)),
            is_failure(&nc.gid_to_name(1001)),
            is_failure(&nc.name_to_uid("alice")),
            is_failure(&nc.name_to_gid("staff")),
        ] {
            assert!(later, "a dead converter must stay dead");
        }

        // ... and it stays dead of the SAME cause. A converter whose death was
        // not recorded would be spoken to again and report whatever the pipe
        // said this time (an EPIPE from writing to a reader that is gone),
        // which is the tell that the stream was retried rather than known to
        // be finished.
        match nc.uid_to_name(1002) {
            ConverterOutcome::Failed(err) => assert_eq!(
                err.kind(),
                recorded,
                "the recorded cause must be reported, not a fresh one: {err}"
            ),
            other => panic!("a dead converter must keep failing: {other:?}"),
        }
    }

    /// The fail-closed rule as the lookup API sees it: a dead converter is an
    /// error, and the error survives the tolerance that a host-database
    /// failure is granted.
    #[cfg(unix)]
    #[test]
    fn a_dead_converter_is_an_error_at_the_lookup_api() {
        let nc = NameConverter::spawn("exit 0").expect("spawn should succeed");
        let _guard = install_name_converter(nc);

        let err = metadata::id_lookup::lookup_user_by_name(b"alice")
            .expect_err("a dead converter must not resolve to `no such user`");
        assert!(err.is_converter_failure());

        // Non-vacuity for the tolerance helper: `no_id_unless_converter_failed`
        // turns a database failure into `None`, and must NOT do that here.
        assert!(
            metadata::id_lookup::no_id_unless_converter_failed(
                metadata::id_lookup::lookup_user_by_name(b"alice")
            )
            .is_err(),
            "the fail-closed rule must survive the database-failure tolerance"
        );
    }

    /// Spawns a converter that answers per-name AND records one byte per request
    /// it actually receives.
    ///
    /// Counting received requests is what makes the guard observable: a name
    /// that is refused *before the write* must produce zero requests. Asserting
    /// only the returned `None` cannot tell that apart from a request that was
    /// sent and answered "unknown", which is how a guard that does nothing still
    /// looks correct.
    #[cfg(unix)]
    fn spawn_counting_converter(log: &std::path::Path) -> (NameConverter, std::path::PathBuf) {
        let log = log.to_path_buf();
        // The count is written BEFORE the answer, so once a reply has been read
        // the corresponding request is already on disk - no polling needed.
        let script = format!(
            r#"while read cmd arg; do
                printf 'x' >> '{}'
                case "$arg" in
                    alice) echo 1001 ;;
                    root)  echo 0 ;;
                    plus)  echo '+5' ;;
                    *)     echo '' ;;
                esac
            done"#,
            log.display()
        );
        (
            NameConverter::spawn(&script).expect("spawn should succeed"),
            log,
        )
    }

    #[cfg(unix)]
    fn requests_seen(log: &std::path::Path) -> usize {
        std::fs::read(log).map(|bytes| bytes.len()).unwrap_or(0)
    }

    #[cfg(unix)]
    #[test]
    fn a_newline_in_a_name_desynchronises_nothing_because_it_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut nc, log) = spawn_counting_converter(&dir.path().join("seen"));

        // Injection attempt: the tail `usr root` would be a SECOND request line.
        let outcome = nc.name_to_uid("zz\nusr root");
        assert!(
            matches!(outcome, ConverterOutcome::Refused),
            "a control byte is refused before the write: {outcome:?}"
        );
        assert_eq!(requests_seen(&log), 0, "the request must never be written");

        // THE ASSERTION THAT MATTERS. Unpatched, the converter answered twice
        // ('' for `zz`, then 0 for `root`) while query() read once - so this
        // lookup consumes the stale `0` and alice resolves to root. Every later
        // lookup in the session stays shifted by one.
        assert_eq!(
            answered(nc.name_to_uid("alice")),
            Some(1001),
            "a refused name must not leave a surplus answer buffered for the next lookup"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_whole_control_range_is_refused_not_just_newline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut nc, log) = spawn_counting_converter(&dir.path().join("seen"));

        // upstream refuses the class (`< ' '` or 0x7f), not just the byte that
        // happens to be exploitable. Only `\n` desynchronises the stream, so
        // the returned outcome is NOT discriminating for these - the request
        // count is. Each must be refused before any write.
        for name in ["a\rb", "a\tb", "a\u{7f}b", "a\u{1}b"] {
            assert!(
                matches!(nc.name_to_uid(name), ConverterOutcome::Refused),
                "{name:?} must be refused"
            );
            assert_eq!(requests_seen(&log), 0, "{name:?} must not be written");
        }

        // Non-vacuity control: an ordinary name IS sent, so the counter above
        // is actually capable of moving.
        assert_eq!(answered(nc.name_to_uid("alice")), Some(1001));
        assert_eq!(requests_seen(&log), 1, "a safe name reaches the converter");
    }

    #[cfg(unix)]
    #[test]
    fn a_refused_token_is_reported_not_merely_unresolved() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut nc, log) = spawn_counting_converter(&dir.path().join("seen"));

        // Refusing silently is a defect of its own: an operator reading the
        // daemon log sees a name that "has no id", indistinguishable from an
        // ordinary unknown user, when in fact a peer sent a control byte.
        // upstream reports it before returning False
        // (clientserver.c:1317-1319 `rprintf(FERROR, "invalid name-converter
        // token: %s\n", *name_p)`).
        let _ = logging::drain_events_for_daemon_log();
        assert!(matches!(
            nc.name_to_uid("bad\nname"),
            ConverterOutcome::Refused
        ));
        assert_eq!(requests_seen(&log), 0, "nothing may reach the converter");

        let reported: Vec<String> = logging::drain_events_for_daemon_log()
            .into_iter()
            .map(|event| {
                let (logging::DiagnosticEvent::Info { message, .. }
                | logging::DiagnosticEvent::Debug { message, .. }) = event;
                message
            })
            .collect();
        assert!(
            reported
                .iter()
                .any(|line| line == "invalid name-converter token: bad\nname"),
            "the refusal must be reported verbatim: {reported:?}"
        );

        // Non-vacuity: an accepted name reaches the converter and reports
        // nothing, so the assertion above cannot pass on an emitter that
        // fires for every lookup.
        assert_eq!(answered(nc.name_to_uid("alice")), Some(1001));
        assert!(
            logging::drain_events_for_daemon_log().is_empty(),
            "an accepted name must not be reported"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_id_answer_must_be_all_digits_on_the_live_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut nc, _log) = spawn_counting_converter(&dir.path().join("seen"));

        // Exercised through name_to_uid, not the helper directly: a test that
        // calls parse_converter_id() alone still passes when the production
        // path is wired straight to str::parse, which is the mistake to catch.
        // upstream clientserver.c:1347-1350 walks the bytes explicitly, so the
        // leading `+` that Rust's parse::<u32> accepts must be refused here.
        let outcome = nc.name_to_uid("plus");
        assert!(
            matches!(outcome, ConverterOutcome::Unknown),
            "`+5` is a malformed answer, which upstream reports as False (not a \
             transport failure): {outcome:?}"
        );
        assert_eq!(
            answered(nc.name_to_uid("alice")),
            Some(1001),
            "stream still in sync"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_id_parser_matches_upstreams_digit_rule() {
        for answer in ["+5", " 5", "5x", "-1", "", "99999999999999999999"] {
            assert_eq!(
                parse_converter_id(answer),
                None,
                "{answer:?} is not a valid id"
            );
        }
        assert_eq!(parse_converter_id("0"), Some(0));
        assert_eq!(parse_converter_id("4294967295"), Some(u32::MAX));
    }

    #[cfg(unix)]
    #[test]
    fn a_request_that_fills_the_buffer_is_refused_before_the_write() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut nc, log) = spawn_counting_converter(&dir.path().join("seen"));

        // "usr " + name + "\n" == NAMECVT_REQUEST_LIMIT exactly, which upstream
        // already treats as truncated (`len >= sizeof buf`) and answers with
        // `exit_cleanup(RERR_UNSUPPORTED)` - a failure, not a name with no id.
        let exact = "n".repeat(NAMECVT_REQUEST_LIMIT - "usr ".len() - 1);
        let outcome = nc.name_to_uid(&exact);
        assert!(is_failure(&outcome), "a full buffer is truncation");
        assert_eq!(requests_seen(&log), 0, "and nothing is written");

        // One byte shorter fits, so it IS sent - and the converter is still
        // usable, because an over-long request never touched the stream.
        let under = "n".repeat(NAMECVT_REQUEST_LIMIT - "usr ".len() - 2);
        assert!(
            matches!(nc.name_to_uid(&under), ConverterOutcome::Unknown),
            "unknown, but it WAS sent"
        );
        assert_eq!(requests_seen(&log), 1, "one byte shorter still fits");
    }

    #[cfg(unix)]
    #[test]
    fn converter_intercepts_lookup_functions() {
        let nc = NameConverter::spawn(
            r#"while read cmd arg; do
                case "$cmd" in
                    uid) echo "mapped_user" ;;
                    gid) echo "mapped_group" ;;
                    usr) echo "5000" ;;
                    grp) echo "6000" ;;
                esac
            done"#,
        )
        .expect("spawn should succeed");

        let _guard = install_name_converter(nc);

        let user_name = metadata::id_lookup::lookup_user_name(1000).unwrap();
        assert_eq!(user_name, Some(b"mapped_user".to_vec()));

        let group_name = metadata::id_lookup::lookup_group_name(1000).unwrap();
        assert_eq!(group_name, Some(b"mapped_group".to_vec()));

        let uid = metadata::id_lookup::lookup_user_by_name(b"alice").unwrap();
        assert_eq!(uid, Some(5000));

        let gid = metadata::id_lookup::lookup_group_by_name(b"staff").unwrap();
        assert_eq!(gid, Some(6000));
    }

    #[cfg(unix)]
    #[test]
    fn guard_clears_converter_on_drop() {
        let nc = NameConverter::spawn("while read cmd arg; do echo testuser; done")
            .expect("spawn should succeed");

        {
            let _guard = install_name_converter(nc);
            let result = metadata::id_lookup::lookup_user_name(42).unwrap();
            assert_eq!(result, Some(b"testuser".to_vec()));
        }

        // After guard is dropped, converter is cleared - falls back to NSS
        let result = metadata::id_lookup::lookup_user_name(999_999_999).unwrap();
        // NSS lookup for non-existent UID returns None
        assert_eq!(result, None);
    }

    #[cfg(unix)]
    #[test]
    fn non_numeric_response_for_name_to_uid_returns_none() {
        let mut nc = NameConverter::spawn("while read cmd arg; do echo 'not_a_number'; done")
            .expect("spawn should succeed");
        assert_eq!(answered(nc.name_to_uid("alice")), None);
    }

    #[cfg(unix)]
    #[test]
    fn converter_handles_protocol_commands() {
        // Realistic converter that dispatches on command type
        let mut nc = NameConverter::spawn(
            r#"while read cmd arg; do
                case "$cmd" in
                    uid) if [ "$arg" = "1000" ]; then echo alice; else echo ""; fi ;;
                    gid) if [ "$arg" = "100" ]; then echo users; else echo ""; fi ;;
                    usr) if [ "$arg" = "alice" ]; then echo 1000; else echo ""; fi ;;
                    grp) if [ "$arg" = "users" ]; then echo 100; else echo ""; fi ;;
                    *) echo "" ;;
                esac
            done"#,
        )
        .expect("spawn should succeed");

        assert_eq!(answered(nc.uid_to_name(1000)), Some("alice".to_owned()));
        assert_eq!(answered(nc.uid_to_name(9999)), None);
        assert_eq!(answered(nc.gid_to_name(100)), Some("users".to_owned()));
        assert_eq!(answered(nc.gid_to_name(9999)), None);
        assert_eq!(answered(nc.name_to_uid("alice")), Some(1000));
        assert_eq!(answered(nc.name_to_uid("nobody")), None);
        assert_eq!(answered(nc.name_to_gid("users")), Some(100));
        assert_eq!(answered(nc.name_to_gid("nobody")), None);
    }
}
