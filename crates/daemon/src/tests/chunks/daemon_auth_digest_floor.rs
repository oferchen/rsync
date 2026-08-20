/// Spins up a one-shot daemon serving an auth-protected `protected` module with
/// an `auth digest = <floor>` line, granting `alice:correctpassword`.
///
/// Deliberately a near-copy of `start_auth_digest_daemon` rather than a
/// parameter added to it: that helper pins the *no-floor* configuration used by
/// the negotiation tests, and threading an option through it would let a future
/// edit change what those tests exercise without touching them.
fn start_auth_digest_floor_daemon(
    dir: &Path,
    floor: &str,
) -> (TcpStream, thread::JoinHandle<Result<(), crate::DaemonError>>) {
    let module_dir = dir.join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let secrets_path = dir.join("secrets.txt");
    fs::write(&secrets_path, "alice:correctpassword\n").expect("write secrets");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o600)).expect("chmod secrets");
    }

    let config_path = dir.join("rsyncd.conf");
    fs::write(
        &config_path,
        format!(
            "[protected]\npath = {}\nauth users = alice\nsecrets file = {}\nuse chroot = false\nauth digest = {floor}\n",
            module_dir.display(),
            secrets_path.display()
        ),
    )
    .expect("write config");

    let (port, held_listener) = allocate_test_port();
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--no-detach"),
            OsString::from("--once"),
            OsString::from("--config"),
            config_path.as_os_str().to_os_string(),
        ])
        .build();

    start_daemon(config, port, held_listener)
}

/// A client that can only offer a digest weaker than the module's floor is
/// refused, and - the part that matters - refused *without a challenge*.
///
/// Asserting only "auth did not succeed" would pass even if the daemon issued a
/// challenge and then rejected the response, which is precisely the outcome the
/// floor exists to prevent: a captured challenge-response pair is what an
/// attacker takes away to brute-force offline, and a weak digest is what makes
/// that cheap. The assertion is therefore on the answer line itself.
///
/// upstream: authenticate.c:324-332 returns NULL before `gen_challenge()`
/// (:334), and clientserver.c:812 turns that into `@ERROR: auth failed on
/// module %s`.
#[test]
fn auth_digest_floor_refuses_a_weak_client_without_sending_a_challenge() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let (mut stream, handle) = start_auth_digest_floor_daemon(dir.path(), "sha256");
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let answer = negotiate_protected_module(&mut stream, &mut reader, "@RSYNCD: 32.0 md5\n");

    assert!(
        !answer.starts_with("@RSYNCD: AUTHREQD"),
        "a challenge must not be sent to a client below the floor, got: {answer}"
    );
    assert!(
        answer.starts_with("@ERROR: auth failed on module protected"),
        "expected upstream's auth-failure line, got: {answer}"
    );

    drop(reader);
    drop(stream);
    if let Some(result) = finish_daemon(handle) {
        assert!(result.is_ok(), "daemon should exit cleanly: {result:?}");
    }
}

/// The positive control for the test above: with the same `auth digest = sha256`
/// floor, a client offering a digest at or above it authenticates normally.
///
/// Without this, a floor that refused *every* client would pass the refusal test
/// and look correct.
#[test]
fn auth_digest_floor_admits_a_client_at_or_above_the_floor() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let (mut stream, handle) = start_auth_digest_floor_daemon(dir.path(), "sha256");
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let answer = negotiate_protected_module(&mut stream, &mut reader, "@RSYNCD: 32.0 sha512\n");
    let challenge = answer
        .strip_prefix("@RSYNCD: AUTHREQD ")
        .unwrap_or_else(|| panic!("expected auth challenge, got: {answer}"));

    let response = core::auth::compute_daemon_auth_response(
        b"correctpassword",
        challenge,
        DaemonAuthDigest::Sha512,
    );
    stream
        .write_all(format!("alice {response}\n").as_bytes())
        .expect("send response");
    stream.flush().expect("flush response");

    let mut line = String::new();
    reader.read_line(&mut line).expect("auth verdict");
    assert_eq!(line.trim_end(), "@RSYNCD: OK");

    drop(reader);
    drop(stream);
    if let Some(result) = finish_daemon(handle) {
        assert!(result.is_ok(), "daemon should exit cleanly: {result:?}");
    }
}

/// An `auth digest` naming a digest this build does not support refuses the
/// connection rather than being ignored - an unrecognised floor must never fail
/// open into "no floor".
///
/// upstream: authenticate.c:316-322, the `floor_rank < 0` arm. Note that the
/// daemon itself still started and parsed the config: upstream discovers this at
/// connection time, per module, so one module's typo cannot take the daemon down.
#[test]
fn auth_digest_floor_naming_an_unsupported_digest_refuses_rather_than_fails_open() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let (mut stream, handle) = start_auth_digest_floor_daemon(dir.path(), "no-such-digest");
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    // sha512 is the strongest digest there is, so a client offering it would
    // clear any real floor. It is refused here only because the floor itself is
    // unresolvable.
    let answer = negotiate_protected_module(&mut stream, &mut reader, "@RSYNCD: 32.0 sha512\n");

    assert!(
        !answer.starts_with("@RSYNCD: AUTHREQD"),
        "an unresolvable floor must not fall through to a challenge, got: {answer}"
    );
    assert!(
        answer.starts_with("@ERROR: auth failed on module protected"),
        "expected upstream's auth-failure line, got: {answer}"
    );

    drop(reader);
    drop(stream);
    if let Some(result) = finish_daemon(handle) {
        assert!(result.is_ok(), "daemon should exit cleanly: {result:?}");
    }
}

/// A module with no `auth digest` keeps upstream's default of no floor, so the
/// md5 a pre-3.2.0 client falls back to still authenticates.
///
/// This is the byte-neutrality guard for every existing deployment: the feature
/// must be inert until an operator opts in.
#[test]
fn no_auth_digest_directive_imposes_no_floor() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let (mut stream, handle) = start_auth_digest_daemon(dir.path());
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let answer = negotiate_protected_module(&mut stream, &mut reader, "@RSYNCD: 32.0 md5\n");
    assert!(
        answer.starts_with("@RSYNCD: AUTHREQD "),
        "an unconfigured module must still challenge an md5-only client, got: {answer}"
    );

    drop(reader);
    drop(stream);
    if let Some(result) = finish_daemon(handle) {
        assert!(result.is_ok(), "daemon should exit cleanly: {result:?}");
    }
}

/// The whole decision surface, every floor against every negotiated digest.
///
/// Upstream's rule is one comparison - `got_rank > floor_rank` refuses
/// (authenticate.c:330) - so the expected value is derivable rather than
/// enumerated: a digest clears a floor exactly when it is at least as strong.
/// Writing it that way means the table cannot disagree with itself, and any
/// change to the *ordering* is caught by
/// `core::auth::tests::strength_rank_matches_supported_order` instead.
#[test]
fn auth_digest_floor_admits_exactly_the_digests_at_or_above_it() {
    for floor in SUPPORTED_DAEMON_DIGESTS {
        for negotiated in SUPPORTED_DAEMON_DIGESTS {
            let clears = negotiated.strength_rank() <= floor.strength_rank();
            let outcome = enforce_auth_digest_floor(Some(floor.name()), *negotiated);
            assert_eq!(
                outcome.is_ok(),
                clears,
                "floor {} vs negotiated {}: expected clears={clears}, got {outcome:?}",
                floor.name(),
                negotiated.name(),
            );
            if !clears {
                assert_eq!(
                    outcome,
                    Err(AuthDenial::DigestTooWeak {
                        negotiated: *negotiated,
                        floor: floor.name().to_owned(),
                    })
                );
            }
        }
    }
}

/// No floor configured admits every digest - including the weakest, which is
/// where a peer that advertises no list lands.
#[test]
fn absent_auth_digest_floor_admits_every_digest() {
    for negotiated in SUPPORTED_DAEMON_DIGESTS {
        assert!(
            enforce_auth_digest_floor(None, *negotiated).is_ok(),
            "no floor must admit {}",
            negotiated.name()
        );
    }
    assert!(enforce_auth_digest_floor(None, DaemonAuthDigest::Md4Old).is_ok());
}

/// `Md4Old` is not an advertisable name but *is* a reachable negotiated value:
/// it is what a peer sending no digest list resolves to below protocol 30
/// (compat.c:879-881). It must be ranked as the `md4` it spells, so an
/// `auth digest = md4` floor admits it and any stronger floor refuses it.
///
/// Ranking it as unknown would refuse it under *every* floor, silently locking
/// out exactly the old clients the directive is written to reason about.
#[test]
fn md4_old_is_treated_as_md4_by_the_floor() {
    assert!(enforce_auth_digest_floor(Some("md4"), DaemonAuthDigest::Md4Old).is_ok());
    assert_eq!(
        enforce_auth_digest_floor(Some("md5"), DaemonAuthDigest::Md4Old),
        Err(AuthDenial::DigestTooWeak {
            negotiated: DaemonAuthDigest::Md4Old,
            floor: "md5".to_owned(),
        })
    );
}

/// An unresolvable floor refuses regardless of what was negotiated - the
/// fail-closed arm. Paired with the `sha512` case so the refusal cannot be
/// mistaken for an ordinary too-weak verdict.
#[test]
fn unresolvable_auth_digest_floor_refuses_every_digest() {
    for negotiated in SUPPORTED_DAEMON_DIGESTS {
        assert_eq!(
            enforce_auth_digest_floor(Some("sponge"), *negotiated),
            Err(AuthDenial::DigestFloorUnsupported {
                configured: "sponge".to_owned(),
            }),
            "an unknown floor must refuse {}",
            negotiated.name()
        );
    }
}

/// Upstream matches the floor name with `strcasecmp` (checksum.c:104), so the
/// case an operator writes must not decide whether their floor works.
#[test]
fn auth_digest_floor_name_is_case_insensitive() {
    assert!(enforce_auth_digest_floor(Some("SHA256"), DaemonAuthDigest::Sha512).is_ok());
    assert!(enforce_auth_digest_floor(Some("Sha256"), DaemonAuthDigest::Md5).is_err());
}

/// The two refusal arms carry upstream's wording into the log line; the
/// credential arm deliberately carries none, leaving the bare prefix.
///
/// upstream: authenticate.c:318-321 and :327-331.
#[test]
fn auth_denial_log_reasons_match_upstream_wording() {
    assert_eq!(AuthDenial::Credentials.log_suffix(), None);
    assert_eq!(
        AuthDenial::DigestFloorUnsupported {
            configured: "sponge".to_owned()
        }
        .log_suffix()
        .as_deref(),
        Some(": the configured 'auth digest = sponge' is not a supported digest on this build")
    );
    assert_eq!(
        AuthDenial::DigestTooWeak {
            negotiated: DaemonAuthDigest::Md5,
            floor: "sha256".to_owned(),
        }
        .log_suffix()
        .as_deref(),
        Some(": negotiated auth digest md5 is weaker than the required 'auth digest = sha256'")
    );
}
