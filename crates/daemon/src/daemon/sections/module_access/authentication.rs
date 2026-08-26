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
    /// Authentication was denied; the variant says why.
    Denied(AuthDenial),
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

/// Why a module authentication attempt was denied.
///
/// Upstream appends the specific reason to its
/// `auth failed on module %s from %s (%s)` FLOG line. Carrying it in the result
/// is what lets the emission point - which owns the log sink, unlike this layer -
/// reproduce that suffix instead of logging the bare prefix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AuthDenial {
    /// Credentials were absent, malformed, or the secret did not verify.
    ///
    /// The first two arrive before a username exists, so upstream's ` for %s:`
    /// clause has nothing to name. The third - a failed secret check - does
    /// have a username, but upstream's reason there is `check_secret()`'s
    /// return ("no secrets file", "ignoring secrets file", "invalid username",
    /// ...) and oc's `verify_secret_response` collapses all of them into one
    /// bool, so naming any single reason would be a guess. These stay the bare
    /// prefix until that outcome is split.
    Credentials,
    /// An `auth users` rule refused the offered username.
    ///
    /// upstream: authenticate.c:433 logs these as
    /// `auth failed on module %s from %s (%s) for %s: %s`, and the reason is
    /// the discriminator an operator needs - see [`AuthUserRule`].
    UserRule { user: String, rule: AuthUserRule },
    /// `auth digest = NAME` names a digest this build does not support.
    ///
    /// upstream: authenticate.c:316-322 - the `floor_rank < 0` arm.
    DigestFloorUnsupported { configured: String },
    /// The negotiated digest is weaker than the module's `auth digest` floor.
    ///
    /// upstream: authenticate.c:324-332 - the `got_rank > floor_rank` arm.
    DigestTooWeak {
        negotiated: DaemonAuthDigest,
        floor: String,
    },
}

/// Which `auth users` rule outcome refused the username.
///
/// A closed set, not free text: these are the only two reasons upstream can
/// report from the rule scan, and conflating them is the exact failure the
/// upstream test guards against - "no matching rule" is what a rule list that
/// parsed to nothing produces, so an operator seeing it knows the policy never
/// matched, while "denied by rule" means the policy matched and did its job.
///
/// upstream: authenticate.c:411-414.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AuthUserRule {
    /// No `auth users` entry matched the offered username.
    ///
    /// upstream: authenticate.c:412 - `if (!tok) err = "no matching rule";`
    NoMatchingRule,
    /// A matching entry carried the `:deny` modifier.
    ///
    /// upstream: authenticate.c:414 - `else if (opt_ch == 'd') err = "denied by
    /// rule";`
    DeniedByRule,
}

impl AuthUserRule {
    /// upstream's literal reason text for this outcome.
    const fn reason(self) -> &'static str {
        match self {
            Self::NoMatchingRule => "no matching rule",
            Self::DeniedByRule => "denied by rule",
        }
    }
}

impl AuthDenial {
    /// The text upstream appends after `auth failed on module %s from %s (%s)`,
    /// or `None` when this layer cannot reconstruct it.
    ///
    /// Returns the WHOLE suffix rather than just a reason, because upstream has
    /// two shapes: `: %s` for the digest-floor refusals (authenticate.c:318 /
    /// :325) and ` for %s: %s` once a username is known (:433). Handing the
    /// emitter a ready-made suffix keeps that choice here, next to the variants
    /// that determine it.
    pub(crate) fn log_suffix(&self) -> Option<String> {
        match self {
            Self::Credentials => None,
            Self::UserRule { user, rule } => Some(format!(" for {user}: {}", rule.reason())),
            Self::DigestFloorUnsupported { configured } => Some(format!(
                ": the configured 'auth digest = {configured}' is not a supported digest on this build"
            )),
            Self::DigestTooWeak { negotiated, floor } => Some(format!(
                ": negotiated auth digest {} is weaker than the required 'auth digest = {floor}'",
                negotiated.name()
            )),
        }
    }
}

/// Applies the module's `auth digest` floor to the digest just negotiated.
///
/// `Ok(())` when no floor is configured or the negotiated digest meets it;
/// `Err` when the connection must be refused.
///
/// upstream: authenticate.c:307-332. Two properties of that placement are
/// load-bearing and mirrored by the caller:
///
/// - it runs *after* `negotiate_daemon_auth()`, so the floor is applied to what
///   was actually negotiated - including the md5/md4 fallback a peer that sends
///   no digest list lands on, which is the downgrade being defended against;
/// - it runs *before* `gen_challenge()`, so a refused peer never receives a
///   challenge it could take away and brute-force offline against a weak digest.
///
/// Rank ordering is upstream's: lower rank is stronger, so a *higher* rank than
/// the floor is too weak.
pub(crate) fn enforce_auth_digest_floor(
    configured_floor: Option<&str>,
    negotiated: DaemonAuthDigest,
) -> Result<(), AuthDenial> {
    let Some(floor) = configured_floor else {
        return Ok(());
    };
    let Some(floor_digest) = daemon_auth_digest_by_name(floor) else {
        return Err(AuthDenial::DigestFloorUnsupported {
            configured: floor.to_owned(),
        });
    };
    if negotiated.strength_rank() > floor_digest.strength_rank() {
        return Err(AuthDenial::DigestTooWeak {
            negotiated,
            floor: floor.to_owned(),
        });
    }
    Ok(())
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

    // upstream: authenticate.c:307-332 - the floor sits here, after the digest
    // is negotiated and before any challenge is generated.
    if let Err(denial) = enforce_auth_digest_floor(module.auth_digest.as_deref(), digest) {
        return Ok(AuthenticationStatus::Denied(denial));
    }

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
        return Ok(AuthenticationStatus::Denied(AuthDenial::Credentials));
    };

    // Parse `<user> <response>` via the shared protocol helper - the exact
    // inverse of the client's `send_daemon_auth_credentials` emit.
    let (username, response_digest) = parse_daemon_auth_response(&response);

    if username.is_empty() || response_digest.is_empty() {
        return Ok(AuthenticationStatus::Denied(AuthDenial::Credentials));
    }

    let auth_match = match module.get_auth_user(username) {
        Some(matched) => matched,
        None => {
            // upstream: authenticate.c:412 - the rule scan ended with no token,
            // so `err = "no matching rule"`.
            return Ok(AuthenticationStatus::Denied(AuthDenial::UserRule {
                user: username.to_owned(),
                rule: AuthUserRule::NoMatchingRule,
            }));
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

    if !verify_secret_response(
        module,
        username,
        auth_group,
        &challenge,
        response_digest,
        digest,
    )? {
        return Ok(AuthenticationStatus::Denied(AuthDenial::Credentials));
    }

    // upstream: authenticate.c:334-335 - `opt_ch == 'd'` ("deny") reports
    // "denied by rule" and auth_server() returns NULL (auth failure).
    if auth_user.access_level == UserAccessLevel::Deny {
        return Ok(AuthenticationStatus::Denied(AuthDenial::UserRule {
            user: username.to_owned(),
            rule: AuthUserRule::DeniedByRule,
        }));
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
    // propagating an io::Error, so the caller emits the @ERROR line on the
    // Denied arm instead of dropping the socket mid-handshake.
    if module.strict_modes && check_secrets_file_permissions(secrets_path).is_err() {
        return Ok(false);
    }

    // upstream: authenticate.c:159 opens the secrets file through
    // `open_no_attacker_symlinks()`. Without the walk, a symlink planted at any
    // component redirects the daemon's privileged read - and because the
    // strict-modes `fstat` above inspects the *target* inode, a link to a
    // root-owned 0600 file passes the mode check while feeding an
    // attacker-chosen file to the password comparison.
    let contents = match crate::daemon::operator_file::read_to_string(secrets_path) {
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
