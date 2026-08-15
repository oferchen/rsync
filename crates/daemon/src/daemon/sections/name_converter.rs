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
    stdin: io::BufWriter<std::process::ChildStdin>,
    stdout: io::BufReader<std::process::ChildStdout>,
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
            stdin,
            stdout,
        })
    }

    /// Sends a query to the converter and reads one line of response.
    ///
    /// `arg` is rejected outright when it carries a control byte: the request
    /// framing is one line per query, so an embedded newline would make the
    /// converter answer twice while this reads once, leaving the surplus answer
    /// buffered for the *next* lookup to consume.
    fn query(&mut self, cmd: &str, arg: &str) -> Option<String> {
        if !is_safe_token(arg) {
            return None;
        }
        let request = format!("{cmd} {arg}\n");
        // upstream: clientserver.c:1322 - `len >= (int)sizeof buf` on a 1024-byte
        // buffer, so 1024 itself is already too long (snprintf truncated it).
        if request.len() >= NAMECVT_REQUEST_LIMIT {
            return None;
        }
        if self.stdin.write_all(request.as_bytes()).is_err() {
            return None;
        }
        if self.stdin.flush().is_err() {
            return None;
        }
        let mut line = String::new();
        match self.stdout.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => {
                let trimmed = line.trim_end().to_owned();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            }
        }
    }

    /// Converts a numeric UID to a username.
    fn uid_to_name(&mut self, uid: u32) -> Option<String> {
        self.query("uid", &uid.to_string())
    }

    /// Converts a numeric GID to a group name.
    fn gid_to_name(&mut self, gid: u32) -> Option<String> {
        self.query("gid", &gid.to_string())
    }

    /// Converts a username to a numeric UID.
    fn name_to_uid(&mut self, name: &str) -> Option<u32> {
        parse_converter_id(&self.query("usr", name)?)
    }

    /// Converts a group name to a numeric GID.
    fn name_to_gid(&mut self, name: &str) -> Option<u32> {
        parse_converter_id(&self.query("grp", name)?)
    }
}

/// Upper bound on a converter request, matching upstream's `char buf[1024]`.
///
/// upstream: clientserver.c:1313 - `char buf[1024]`, refused at `:1322` once the
/// formatted request reaches that length.
#[cfg(unix)]
const NAMECVT_REQUEST_LIMIT: usize = 1024;

/// Rejects a name that cannot be framed as a single request line.
///
/// upstream: clientserver.c:1362 `namecvt_safe_token()` - refuses any byte below
/// `' '` or equal to `0x7f`, checked at `:1317` *before* the request is
/// formatted. Newline is the byte that matters (it desynchronises the
/// request/answer stream); the rest of the control range comes along because
/// upstream refuses the whole class rather than one byte.
#[cfg(unix)]
fn is_safe_token(name: &str) -> bool {
    !name.bytes().any(|byte| byte < b' ' || byte == 0x7f)
}

/// Parses a converter's id answer under upstream's strict digit rule.
///
/// upstream: clientserver.c:1341-1355 - an empty line, any non-digit byte, or a
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
    fn uid_to_name(&mut self, uid: u32) -> Option<String> {
        self.uid_to_name(uid)
    }
    fn gid_to_name(&mut self, gid: u32) -> Option<String> {
        self.gid_to_name(gid)
    }
    fn name_to_uid(&mut self, name: &str) -> Option<u32> {
        self.name_to_uid(name)
    }
    fn name_to_gid(&mut self, name: &str) -> Option<u32> {
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
    fn uid_to_name(&mut self, uid: u32) -> Option<String> {
        platform::name_resolution::rid_to_account_name(uid)
    }
    fn gid_to_name(&mut self, gid: u32) -> Option<String> {
        // On Windows, GIDs map to group RIDs. Resolve them through the same
        // rid_to_account_name path used for user RIDs, which matches accounts
        // by RID regardless of whether they name a user or a local group.
        platform::name_resolution::rid_to_account_name(gid)
    }
    fn name_to_uid(&mut self, name: &str) -> Option<u32> {
        platform::name_resolution::name_to_rid(name)
    }
    fn name_to_gid(&mut self, name: &str) -> Option<u32> {
        platform::name_resolution::lookup_account_info(name).map(|(rid, _)| rid)
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

    #[cfg(unix)]
    #[test]
    fn spawn_and_query_uid_to_name() {
        // Simple script that echoes a fixed name for any uid query
        let mut nc = NameConverter::spawn("while read cmd arg; do echo testuser; done")
            .expect("spawn should succeed");
        assert_eq!(nc.uid_to_name(1000), Some("testuser".to_owned()));
        assert_eq!(nc.uid_to_name(0), Some("testuser".to_owned()));
    }

    #[cfg(unix)]
    #[test]
    fn spawn_and_query_gid_to_name() {
        let mut nc = NameConverter::spawn("while read cmd arg; do echo testgroup; done")
            .expect("spawn should succeed");
        assert_eq!(nc.gid_to_name(1000), Some("testgroup".to_owned()));
    }

    #[cfg(unix)]
    #[test]
    fn spawn_and_query_name_to_uid() {
        let mut nc = NameConverter::spawn("while read cmd arg; do echo 1001; done")
            .expect("spawn should succeed");
        assert_eq!(nc.name_to_uid("alice"), Some(1001));
    }

    #[cfg(unix)]
    #[test]
    fn spawn_and_query_name_to_gid() {
        let mut nc = NameConverter::spawn("while read cmd arg; do echo 2002; done")
            .expect("spawn should succeed");
        assert_eq!(nc.name_to_gid("staff"), Some(2002));
    }

    #[cfg(unix)]
    #[test]
    fn empty_response_returns_none() {
        let mut nc = NameConverter::spawn("while read cmd arg; do echo ''; done")
            .expect("spawn should succeed");
        assert_eq!(nc.uid_to_name(1000), None);
    }

    #[cfg(unix)]
    #[test]
    fn broken_pipe_returns_none() {
        // Child exits immediately, causing broken pipe on write or EOF on read.
        // The query call blocks on read_line which returns EOF once the child exits.
        let mut nc = NameConverter::spawn("exit 0").expect("spawn should succeed");
        assert_eq!(nc.uid_to_name(1000), None);
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
        assert_eq!(nc.name_to_uid("zz\nusr root"), None);
        assert_eq!(requests_seen(&log), 0, "the request must never be written");

        // THE ASSERTION THAT MATTERS. Unpatched, the converter answered twice
        // ('' for `zz`, then 0 for `root`) while query() read once - so this
        // lookup consumes the stale `0` and alice resolves to root. Every later
        // lookup in the session stays shifted by one.
        assert_eq!(
            nc.name_to_uid("alice"),
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
        // the returned None is NOT discriminating for these - the request count
        // is. Each must be refused before any write.
        for name in ["a\rb", "a\tb", "a\u{7f}b", "a\u{1}b"] {
            assert_eq!(nc.name_to_uid(name), None, "{name:?} must be refused");
            assert_eq!(requests_seen(&log), 0, "{name:?} must not be written");
        }

        // Non-vacuity control: an ordinary name IS sent, so the counter above
        // is actually capable of moving.
        assert_eq!(nc.name_to_uid("alice"), Some(1001));
        assert_eq!(requests_seen(&log), 1, "a safe name reaches the converter");
    }

    #[cfg(unix)]
    #[test]
    fn an_id_answer_must_be_all_digits_on_the_live_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut nc, _log) = spawn_counting_converter(&dir.path().join("seen"));

        // Exercised through name_to_uid, not the helper directly: a test that
        // calls parse_converter_id() alone still passes when the production
        // path is wired straight to str::parse, which is the mistake to catch.
        // upstream clientserver.c:1345-1348 walks the bytes explicitly, so the
        // leading `+` that Rust's parse::<u32> accepts must be refused here.
        assert_eq!(nc.name_to_uid("plus"), None, "`+5` is not a valid id");
        assert_eq!(nc.name_to_uid("alice"), Some(1001), "stream still in sync");
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
        // already treats as truncated (`len >= sizeof buf`). Both sizes return
        // None (neither name is known), so only the request count separates
        // "refused" from "sent and unknown".
        let exact = "n".repeat(NAMECVT_REQUEST_LIMIT - "usr ".len() - 1);
        assert_eq!(nc.name_to_uid(&exact), None);
        assert_eq!(requests_seen(&log), 0, "a full buffer is truncation");

        let under = "n".repeat(NAMECVT_REQUEST_LIMIT - "usr ".len() - 2);
        assert_eq!(nc.name_to_uid(&under), None, "unknown, but it WAS sent");
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
        assert_eq!(nc.name_to_uid("alice"), None);
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

        assert_eq!(nc.uid_to_name(1000), Some("alice".to_owned()));
        assert_eq!(nc.uid_to_name(9999), None);
        assert_eq!(nc.gid_to_name(100), Some("users".to_owned()));
        assert_eq!(nc.gid_to_name(9999), None);
        assert_eq!(nc.name_to_uid("alice"), Some(1000));
        assert_eq!(nc.name_to_uid("nobody"), None);
        assert_eq!(nc.name_to_gid("users"), Some(100));
        assert_eq!(nc.name_to_gid("nobody"), None);
    }
}
