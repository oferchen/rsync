/// Long-lived subprocess providing uid/gid name conversion in chroot environments.
///
/// When a daemon module specifies `name converter`, this subprocess replaces
/// NSS lookups (getpwuid, getpwnam, getgrgid, getgrnam) with a simple
/// line-based protocol over stdin/stdout pipes.
///
/// upstream: clientserver.c:962-969 - the name converter is spawned after
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
    fn query(&mut self, cmd: &str, arg: &str) -> Option<String> {
        let request = format!("{cmd} {arg}\n");
        if request.len() > 1024 {
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
        self.query("usr", name)?.parse().ok()
    }

    /// Converts a group name to a numeric GID.
    fn name_to_gid(&mut self, name: &str) -> Option<u32> {
        self.query("grp", name)?.parse().ok()
    }
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
        // On Windows, GIDs map to group RIDs. Reuse the same enum-and-match
        // approach. For groups, we could enumerate local groups, but the
        // rid_to_account_name only enumerates users. For now, return None
        // for group reverse lookups - groups are less commonly preserved.
        // A full implementation would enumerate via NetLocalGroupEnum.
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
