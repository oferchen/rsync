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
    /// No digest the client offered is implemented here, so the exchange cannot
    /// start. The refusal line has already been written and no challenge was
    /// sent.
    ///
    /// upstream: compat.c:871-875 - `negotiate_daemon_auth()` writes
    /// `@ERROR: your client does not support one of our daemon-auth checksums`
    /// and calls `exit_cleanup(RERR_UNSUPPORTED)` before `auth_server()` ever
    /// reaches `gen_challenge()`.
    DigestUnsupported,
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
/// `client_digests` is the digest name list the client advertised in its
/// `@RSYNCD:` greeting; it fixes the single algorithm used for both the challenge
/// and the verification. An advertised-but-empty list refuses the client -
/// upstream keeps `strdup("")` non-NULL and so never reaches its no-list
/// substitute (compat.c:857-862).
///
/// upstream: authenticate.c:242 - `auth_server()` calls
/// `negotiate_daemon_auth(f_out, 0)` *before* `gen_challenge()`, so a client with
/// no acceptable digest is refused without ever receiving a challenge.
///
/// Returns `Granted` if authentication succeeded, `Denied` on bad credentials, or
/// `DigestUnsupported` when negotiation found no mutual algorithm.
fn perform_module_authentication(
    reader: &mut BufReader<DaemonStream>,
    limiter: &mut Option<BandwidthLimiter>,
    module: &ModuleDefinition,
    peer_ip: IpAddr,
    messages: &LegacyMessageCache,
    protocol_version: Option<ProtocolVersion>,
    client_digests: AdvertisedDigests<'_>,
) -> io::Result<AuthenticationStatus> {
    // upstream: authenticate.c:76 `gen_challenge` and :90 `generate_hash` both
    // call `sum_init(valid_auth_checksums.negotiated_nni, 0)`, so one negotiated
    // digest drives both halves. Deriving it from the response instead would let
    // the client choose which algorithm it is checked against.
    let digest = match negotiate_server_daemon_digest(
        client_digests,
        protocol_version.map_or(ProtocolVersion::NEWEST.as_u8(), |v| v.as_u8()),
    ) {
        Ok(digest) => digest,
        Err(_) => {
            let error = AtError::UnsupportedAuthDigest {
                digests: supported_daemon_digest_list(),
            };
            send_error(reader.get_mut(), limiter, &error)?;
            return Ok(AuthenticationStatus::DigestUnsupported);
        }
    };

    let challenge = ChallengeGenerator::generate(peer_ip, digest);
    {
        let stream = reader.get_mut();
        messages.write(
            stream,
            limiter,
            LegacyDaemonMessage::AuthRequired {
                challenge: Some(&challenge),
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

    // Parse `<user> <response>` via the shared protocol helper - the exact
    // inverse of the client's `send_daemon_auth_credentials` emit.
    let (username, response_digest) = parse_daemon_auth_response(&response);

    if username.is_empty() || response_digest.is_empty() {
        send_auth_failed(reader.get_mut(), module, limiter)?;
        return Ok(AuthenticationStatus::Denied);
    }

    let auth_match = match module.get_auth_user(username) {
        Some(matched) => matched,
        None => {
            send_auth_failed(reader.get_mut(), module, limiter)?;
            return Ok(AuthenticationStatus::Denied);
        }
    };
    let auth_user = auth_match.user;

    // upstream: authenticate.c:318 - check_secret() receives the group name only
    // when the client was authorized via a matching `@group` token in
    // `auth users` (`group_match >= 0 ? auth_uid_groups[group_match] : NULL`).
    // A plain-username authorization passes NULL, so `@group:` secret lines
    // never match. This is the concrete resolved group name (e.g. the
    // `administrators` that a `@admin*` token matched), not the token itself,
    // because upstream keys the `@group:` secrets lookup off the real group.
    let auth_group = auth_match.group.as_deref();

    if !verify_secret_response(module, username, auth_group, &challenge, response_digest, digest)? {
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
/// `digest` is the algorithm fixed by `negotiate_server_daemon_digest`; the same
/// one that produced the challenge, exactly as upstream reuses
/// `valid_auth_checksums.negotiated_nni` for both.
///
/// Returns `true` if a matching key's digest matches, `false` otherwise.
fn verify_secret_response(
    module: &ModuleDefinition,
    username: &str,
    group: Option<&str>,
    challenge: &str,
    response: &str,
    digest: DaemonAuthDigest,
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
        if verify_daemon_auth_response(secret.as_bytes(), challenge, response, digest) {
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
/// upstream: clientserver.c:812 - `@ERROR: auth failed on module %s\n`
fn send_auth_failed(
    stream: &mut DaemonStream,
    module: &ModuleDefinition,
    limiter: &mut Option<BandwidthLimiter>,
) -> io::Result<()> {
    let error = AtError::AuthFailed {
        module: sanitize_module_identifier(&module.name).into_owned(),
    };
    send_error(stream, limiter, &error)
}
