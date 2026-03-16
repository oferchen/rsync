//! Challenge-response authentication for protected modules.
//!
//! upstream: authenticate.c, compat.c:858

/// Result of a module authentication attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
enum AuthenticationStatus {
    /// Authentication was successful, carrying the authenticated username.
    Granted(String),
    /// Authentication was denied (bad credentials or missing response).
    Denied,
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
    reader: &mut BufReader<TcpStream>,
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
        send_auth_failed(reader.get_mut(), module, limiter, messages)?;
        return Ok(AuthenticationStatus::Denied);
    };

    let mut segments = response.splitn(2, |ch: char| ch.is_ascii_whitespace());
    let username = segments.next().unwrap_or_default();
    let digest = segments.next().map_or("", |segment| {
        segment.trim_start_matches(|ch: char| ch.is_ascii_whitespace())
    });

    if username.is_empty() || digest.is_empty() {
        send_auth_failed(reader.get_mut(), module, limiter, messages)?;
        return Ok(AuthenticationStatus::Denied);
    }

    let auth_user = match module.get_auth_user(username) {
        Some(user) => user,
        None => {
            send_auth_failed(reader.get_mut(), module, limiter, messages)?;
            return Ok(AuthenticationStatus::Denied);
        }
    };

    if !verify_secret_response(module, username, &challenge, digest, protocol_version)? {
        send_auth_failed(reader.get_mut(), module, limiter, messages)?;
        return Ok(AuthenticationStatus::Denied);
    }

    // Check for explicit deny access level
    if auth_user.access_level == UserAccessLevel::Deny {
        send_auth_failed(reader.get_mut(), module, limiter, messages)?;
        return Ok(AuthenticationStatus::Denied);
    }

    Ok(AuthenticationStatus::Granted(username.to_owned()))
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
/// Reads the module's secrets file line by line, looking for a matching
/// username entry. For matching usernames, computes the expected digest
/// using the stored secret and challenge, then compares with the client's
/// response.
///
/// When the module has `strict_modes` enabled (the default), the secrets file
/// permissions are validated before reading: the file must not be accessible by
/// "other" users.
///
/// upstream: authenticate.c - `check_secret()` enforces `lp_strict_modes(module)`
/// by rejecting files with `(st.st_mode & 06) != 0`.
///
/// The `protocol_version` is forwarded to `verify_daemon_auth_response` to
/// select the correct digest for ambiguous MD4/MD5 responses.
///
/// Returns `true` if the username exists and the digest matches, `false` otherwise.
fn verify_secret_response(
    module: &ModuleDefinition,
    username: &str,
    challenge: &str,
    response: &str,
    protocol_version: Option<ProtocolVersion>,
) -> io::Result<bool> {
    let secrets_path = match &module.secrets_file {
        Some(path) => path,
        None => return Ok(false),
    };

    if module.strict_modes {
        check_secrets_file_permissions(secrets_path)?;
    }

    let contents = fs::read_to_string(secrets_path)?;

    for raw_line in contents.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some((user, secret)) = line.split_once(':')
            && user == username
            && verify_daemon_auth_response(
                secret.as_bytes(),
                challenge,
                response,
                protocol_version.map(|v| v.as_u8()),
            )
        {
            return Ok(true);
        }
    }

    Ok(false)
}

/// Checks that a secrets file has appropriately restrictive permissions.
///
/// On Unix, verifies the file is not other-accessible (`mode & 0o006`).
/// When the daemon runs as root, also verifies the file is owned by root.
///
/// upstream: authenticate.c - `(st.st_mode & 06) != 0` rejects other-accessible
/// files; `st.st_uid != ROOT_UID` rejects non-root-owned files when running as root.
#[cfg(unix)]
#[allow(unsafe_code)]
fn check_secrets_file_permissions(path: &Path) -> io::Result<()> {
    let metadata = fs::metadata(path)?;
    let mode = metadata.permissions().mode();

    // upstream: authenticate.c - reject if other-readable or other-writable
    if (mode & 0o006) != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "secrets file must not be other-accessible (see strict modes option): '{}'",
                path.display()
            ),
        ));
    }

    // upstream: authenticate.c - when running as root, secrets must be owned by root
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: `getuid` is a trivial POSIX call with no arguments and no side effects.
        let my_uid = unsafe { libc::getuid() };
        if my_uid == 0 && metadata.uid() != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "secrets file must be owned by root when running as root (see strict modes option): '{}'",
                    path.display()
                ),
            ));
        }
    }

    Ok(())
}

/// No-op permission check on non-Unix platforms (matching upstream rsync).
#[cfg(not(unix))]
fn check_secrets_file_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Sends an auth failure response to the client and closes the session.
///
/// upstream: clientserver.c:762 - `@ERROR: auth failed on module %s\n`
fn send_auth_failed(
    stream: &mut TcpStream,
    module: &ModuleDefinition,
    limiter: &mut Option<BandwidthLimiter>,
    messages: &LegacyMessageCache,
) -> io::Result<()> {
    let module_display = sanitize_module_identifier(&module.name);
    let payload = AUTH_FAILED_PAYLOAD.replace("{module}", module_display.as_ref());
    write_limited(stream, limiter, payload.as_bytes())?;
    write_limited(stream, limiter, b"\n")?;
    messages.write_exit(stream, limiter)?;
    stream.flush()
}
