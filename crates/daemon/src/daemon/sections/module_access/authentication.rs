// Challenge-response authentication for protected modules.
//
// Implements the daemon side of the rsync AUTHREQD handshake: the server
// generates a random challenge, sends it to the client, reads back the
// username + hashed response, and verifies against the secrets file.
//
// upstream: authenticate.c - `auth_server()` generates the challenge and
// verifies the client response. compat.c:858 - selects MD5 (protocol >= 30)
// or MD4 (protocol < 30) for the challenge digest.

/// Result of a module authentication attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
enum AuthenticationStatus {
    /// Authentication was successful, carrying the authenticated username and
    /// the per-user access-level override parsed from the `auth users` entry
    /// (`name:ro` / `name:rw`; `Default` when the entry has no such suffix).
    Granted {
        /// The authenticated username.
        username: String,
        /// Per-user access-level override applied to the session's `read only`.
        access_level: UserAccessLevel,
    },
    /// Authentication was denied (bad credentials or missing response).
    Denied,
}

/// Resolves the session's effective `read only` flag after authentication.
///
/// A user listed in `auth users` may carry an access-level suffix that
/// overrides the module's `read only` setting for that session:
///
/// - `name:ro` forces read-only (client pushes are refused).
/// - `name:rw` forces writable (pushes are allowed even on a `read only` module).
/// - no suffix leaves the module's own `read only` in force.
///
/// `name:deny` is handled earlier by refusing authentication outright, so it
/// never reaches this function.
///
/// upstream: authenticate.c:340-343 - `if (opt_ch=='r') read_only=1; else if
/// (opt_ch=='w') read_only=0;`, applied to the `read_only` global that
/// `rsync_module()` seeds from `lp_read_only(module_id)` (clientserver.c:760).
fn access_effective_read_only(module_read_only: bool, access: UserAccessLevel) -> bool {
    match access {
        UserAccessLevel::ReadOnly => true,
        UserAccessLevel::ReadWrite => false,
        UserAccessLevel::Default | UserAccessLevel::Deny => module_read_only,
    }
}

/// Performs challenge-response authentication for a protected module.
///
/// This implements the rsync daemon authentication protocol:
/// 1. Sends a base64-encoded challenge to the client
/// 2. Reads the client's response containing username and digest
/// 3. Verifies the digest against the module's secrets file
///
/// The `protocol_version` determines which digest algorithm to use for legacy
/// (MD4/MD5) responses when the response length is ambiguous.
///
/// upstream: compat.c:858 - `protocol_version >= 30 ? "md5" : "md4"`
///
/// Returns `Granted` if authentication succeeded, `Denied` otherwise.
fn perform_module_authentication(
    reader: &mut BufReader<DaemonStream>,
    limiter: &mut Option<BandwidthLimiter>,
    module: &ModuleDefinition,
    peer_ip: IpAddr,
    messages: &LegacyMessageCache,
    protocol_version: Option<ProtocolVersion>,
) -> io::Result<AuthenticationStatus> {
    let challenge = generate_auth_challenge(peer_ip, protocol_version);
    {
        let stream = reader.get_mut();
        messages.write(
            stream,
            limiter,
            LegacyDaemonMessage::AuthRequired {
                module: Some(&challenge),
            },
        )?;
        stream.flush()?;
    }

    let response = if let Some(line) = read_trimmed_line(reader)? {
        line
    } else {
        send_auth_failed(reader.get_mut(), module, limiter)?;
        return Ok(AuthenticationStatus::Denied);
    };

    let mut segments = response.splitn(2, |ch: char| ch.is_ascii_whitespace());
    let username = segments.next().unwrap_or_default();
    let digest = segments.next().map_or("", |segment| {
        segment.trim_start_matches(|ch: char| ch.is_ascii_whitespace())
    });

    if username.is_empty() || digest.is_empty() {
        send_auth_failed(reader.get_mut(), module, limiter)?;
        return Ok(AuthenticationStatus::Denied);
    }

    let auth_user = match module.get_auth_user(username) {
        Some(user) => user,
        None => {
            send_auth_failed(reader.get_mut(), module, limiter)?;
            return Ok(AuthenticationStatus::Denied);
        }
    };

    // upstream: authenticate.c:318 - check_secret() receives the group name only
    // when the client was authorized via a matching `@group` token in
    // `auth users` (`group_match >= 0 ? auth_uid_groups[group_match] : NULL`).
    // A plain-username authorization passes NULL, so `@group:` secret lines
    // never match. The matched entry's verbatim token carries that group.
    let auth_group = auth_user.username.strip_prefix('@');

    if !verify_secret_response(
        module,
        username,
        auth_group,
        &challenge,
        digest,
        protocol_version,
    )? {
        send_auth_failed(reader.get_mut(), module, limiter)?;
        return Ok(AuthenticationStatus::Denied);
    }

    // upstream: authenticate.c:334-335 - `opt_ch == 'd'` ("deny") reports
    // "denied by rule" and auth_server() returns NULL (auth failure).
    if auth_user.access_level == UserAccessLevel::Deny {
        send_auth_failed(reader.get_mut(), module, limiter)?;
        return Ok(AuthenticationStatus::Denied);
    }

    // upstream: authenticate.c:340-343 - the `:ro` / `:rw` suffix travels back
    // to rsync_module() via the `read_only` global; carry the parsed access
    // level so the caller can apply it to the session's effective `read only`.
    Ok(AuthenticationStatus::Granted {
        username: username.to_owned(),
        access_level: auth_user.access_level,
    })
}

/// Generates a unique authentication challenge string.
///
/// The challenge is created by combining the peer IP address, current timestamp,
/// and process ID, then hashing with the protocol-appropriate digest and encoding
/// as base64. This produces a unique, time-sensitive challenge for each
/// authentication attempt.
///
/// upstream: compat.c:858 - the digest used for the challenge depends on the
/// negotiated protocol version: MD5 for protocol >= 30, MD4 for protocol < 30.
fn generate_auth_challenge(
    peer_ip: IpAddr,
    protocol_version: Option<ProtocolVersion>,
) -> String {
    let mut input = [0u8; 32];
    let address_text = peer_ip.to_string();
    let address_bytes = address_text.as_bytes();
    let copy_len = address_bytes.len().min(16);
    input[..copy_len].copy_from_slice(&address_bytes[..copy_len]);

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = (timestamp.as_secs() & u64::from(u32::MAX)) as u32;
    let micros = timestamp.subsec_micros();
    let pid = std::process::id();

    input[16..20].copy_from_slice(&seconds.to_le_bytes());
    input[20..24].copy_from_slice(&micros.to_le_bytes());
    input[24..28].copy_from_slice(&pid.to_le_bytes());

    let version = protocol_version.map_or(32, |v| v.as_u8());
    let digest = if version >= 30 {
        let mut hasher = Md5::new();
        hasher.update(&input);
        hasher.finalize().to_vec()
    } else {
        let mut hasher = Md4::new();
        hasher.update(&input);
        hasher.finalize().to_vec()
    };
    STANDARD_NO_PAD.encode(digest)
}

/// Verifies a client's authentication response against the secrets file.
///
/// Reads the module's secrets file line by line, mirroring upstream
/// `check_secret()`. A line whose key starts with `@` is matched against the
/// group `auth users` used to authorize the client (`group`, `None` when the
/// client was authorized by a plain-username token); every other line is
/// matched against `username`. This lets a shared `@group:secret` entry
/// authenticate any member of that group, as upstream does.
///
/// First-name-match wins: on the first line whose key matches but whose digest
/// mismatches, that key is retired so later duplicate entries for the same key
/// cannot override the denial. User and group keys are retired independently,
/// exactly as upstream nulls the individual `user`/`group` pointer.
///
/// When the module has `strict_modes` enabled (the default), the secrets file
/// permissions are validated before reading: the file must not be accessible by
/// "other" users.
///
/// upstream: authenticate.c:100-169 - `check_secret()` matches `@group`/user
/// keys, enforces `lp_strict_modes(module)` by rejecting files with
/// `(st.st_mode & 06) != 0`, and on a password mismatch sets `*ptr = NULL`
/// ("Don't look for name again").
///
/// The `protocol_version` is forwarded to `verify_daemon_auth_response` to
/// select the correct digest for ambiguous MD4/MD5 responses.
///
/// Returns `true` if a matching key's digest matches, `false` otherwise.
fn verify_secret_response(
    module: &ModuleDefinition,
    username: &str,
    group: Option<&str>,
    challenge: &str,
    response: &str,
    protocol_version: Option<ProtocolVersion>,
) -> io::Result<bool> {
    let secrets_path = match &module.secrets_file {
        Some(path) => path,
        None => return Ok(false),
    };

    // upstream: authenticate.c:119-131 check_secret() - a strict-modes
    // violation (other-accessible secrets, or non-root ownership when
    // running as root) sets `ok = 0` and returns the "ignoring secrets file"
    // error string; an unreadable secrets file returns "no secrets file".
    // In every case auth_server() reports an auth failure and the client
    // still receives `@ERROR: auth failed on module X`. check_secret() never
    // aborts the connection. Treat these as a denial (Ok(false)) rather than
    // propagating an io::Error, so the daemon emits the @ERROR line via
    // send_auth_failed() instead of dropping the socket mid-handshake.
    if module.strict_modes && check_secrets_file_permissions(secrets_path).is_err() {
        return Ok(false);
    }

    let contents = match fs::read_to_string(secrets_path) {
        Ok(contents) => contents,
        Err(_) => return Ok(false),
    };

    // upstream: authenticate.c:141 `while ((user || group) && ...)` - each key
    // is retired once it mismatches, so scanning stops when neither a user nor
    // a group line can still match.
    let mut user_active = true;
    let mut group_active = group.is_some();

    for raw_line in contents.lines() {
        if !user_active && !group_active {
            break;
        }

        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // upstream: authenticate.c:145-152 - an `@`-prefixed key selects the
        // group pointer (skipping the `@`); every other key selects the user.
        let (active, expected, entry) = match line.strip_prefix('@') {
            Some(rest) => (&mut group_active, group, rest),
            None => (&mut user_active, Some(username), line),
        };

        if !*active {
            continue;
        }

        let Some((key, secret)) = entry.split_once(':') else {
            continue;
        };
        if Some(key) != expected {
            continue;
        }

        // upstream: authenticate.c:158-163 - the first key-matching line decides
        // the outcome; a digest match authenticates, a mismatch retires the key
        // (`*ptr = NULL`) so later duplicates cannot flip the denial.
        if verify_daemon_auth_response(
            secret.as_bytes(),
            challenge,
            response,
            protocol_version.map(|v| v.as_u8()),
        ) {
            return Ok(true);
        }
        *active = false;
    }

    Ok(false)
}

/// Checks that a secrets file has appropriately restrictive permissions.
///
/// Delegates to `platform::secrets::check_secrets_file_permissions()`.
///
/// upstream: authenticate.c - permission checks for secrets files.
fn check_secrets_file_permissions(path: &Path) -> io::Result<()> {
    platform::secrets::check_secrets_file_permissions(path)
}

/// Sends an auth failure response to the client and closes the session.
///
/// upstream: clientserver.c:762 - `@ERROR: auth failed on module %s\n`
fn send_auth_failed(
    stream: &mut DaemonStream,
    module: &ModuleDefinition,
    limiter: &mut Option<BandwidthLimiter>,
) -> io::Result<()> {
    let module_display = sanitize_module_identifier(&module.name);
    let payload = AUTH_FAILED_PAYLOAD.replace("{module}", module_display.as_ref());
    send_error(stream, limiter, &payload)
}
