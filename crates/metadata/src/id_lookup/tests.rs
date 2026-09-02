//! Tests for UID/GID lookup and mapping.

use super::*;
use std::sync::{Mutex, OnceLock};

/// Global lock to serialize tests that modify shared caches.
#[cfg(unix)]
fn cache_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Global lock serializing tests that touch the process-wide name memo and its
/// miss counter (both are shared mutable state).
fn name_cache_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[test]
fn map_uid_numeric_ids_returns_same_uid() {
    let result = map_uid(1000, true);
    assert!(result.is_some());
}

#[test]
fn map_uid_non_numeric_attempts_name_lookup() {
    let result = map_uid(1000, false);
    assert!(result.is_some());
}

#[test]
fn map_uid_nonexistent_uid_falls_back() {
    let result = map_uid(999999, false);
    assert!(result.is_some());
}

#[test]
fn map_gid_numeric_ids_returns_same_gid() {
    let result = map_gid(1000, true);
    assert!(result.is_some());
}

#[test]
fn map_gid_non_numeric_attempts_name_lookup() {
    let result = map_gid(1000, false);
    assert!(result.is_some());
}

#[test]
fn map_gid_nonexistent_gid_falls_back() {
    let result = map_gid(999999, false);
    assert!(result.is_some());
}

#[test]
fn lookup_user_name_root_returns_name() {
    let result = lookup_user_name(0);
    assert!(result.is_ok());
}

#[test]
fn lookup_user_name_nonexistent_uid_returns_none() {
    let result = lookup_user_name(999999999);
    assert!(result.is_ok());
}

#[test]
fn lookup_user_by_name_root_returns_uid() {
    let result = lookup_user_by_name(b"root");
    assert!(result.is_ok());
    if let Ok(Some(uid)) = result {
        assert_eq!(uid, 0);
    }
}

#[test]
fn lookup_user_by_name_nonexistent_returns_none() {
    let result = lookup_user_by_name(b"nonexistent_user_xyz_12345");
    assert!(result.is_ok());
    assert!(result.unwrap().is_none());
}

#[test]
fn lookup_user_by_name_with_null_byte_returns_none() {
    let result = lookup_user_by_name(b"test\x00user");
    assert!(result.is_ok());
    assert!(result.unwrap().is_none());
}

#[test]
fn lookup_user_by_name_empty_returns_none() {
    let result = lookup_user_by_name(b"");
    assert!(result.is_ok());
}

#[test]
fn lookup_group_name_root_group_returns_name() {
    let result = lookup_group_name(0);
    assert!(result.is_ok());
}

#[test]
fn lookup_group_name_nonexistent_gid_returns_none() {
    let result = lookup_group_name(999999999);
    assert!(result.is_ok());
}

#[test]
fn lookup_group_by_name_root_returns_gid() {
    let result = lookup_group_by_name(b"root");
    if result.is_ok() && result.as_ref().unwrap().is_some() {
        assert_eq!(result.unwrap().unwrap(), 0);
    } else {
        // On macOS, root group might be called "wheel"
        let wheel_result = lookup_group_by_name(b"wheel");
        assert!(wheel_result.is_ok());
    }
}

#[test]
fn lookup_group_by_name_nonexistent_returns_none() {
    let result = lookup_group_by_name(b"nonexistent_group_xyz_12345");
    assert!(result.is_ok());
    assert!(result.unwrap().is_none());
}

#[test]
fn lookup_group_by_name_with_null_byte_returns_none() {
    let result = lookup_group_by_name(b"test\x00group");
    assert!(result.is_ok());
    assert!(result.unwrap().is_none());
}

#[test]
fn lookup_group_by_name_empty_returns_none() {
    let result = lookup_group_by_name(b"");
    assert!(result.is_ok());
}

#[test]
fn lookup_user_name_and_by_name_round_trip() {
    if let Ok(Some(name)) = lookup_user_name(0) {
        if let Ok(Some(uid)) = lookup_user_by_name(&name) {
            assert_eq!(uid, 0);
        }
    }
}

#[test]
fn lookup_group_name_and_by_name_round_trip() {
    if let Ok(Some(name)) = lookup_group_name(0) {
        if let Ok(Some(gid)) = lookup_group_by_name(&name) {
            assert_eq!(gid, 0);
        }
    }
}

#[test]
fn map_uid_and_map_gid_consistency() {
    let uid_result = map_uid(1000, true);
    let gid_result = map_gid(1000, true);
    assert!(uid_result.is_some());
    assert!(gid_result.is_some());
}

#[cfg(unix)]
#[test]
fn uid_cache_stores_mapping_on_lookup() {
    let _lock = cache_lock().lock().unwrap();
    clear_id_caches();
    let initial_size = uid_cache_size();

    let _ = map_uid(1000, false);

    assert!(
        uid_cache_size() > initial_size,
        "UID cache should grow after lookup"
    );
}

#[cfg(unix)]
#[test]
fn gid_cache_stores_mapping_on_lookup() {
    let _lock = cache_lock().lock().unwrap();
    clear_id_caches();
    let initial_size = gid_cache_size();

    let _ = map_gid(1000, false);

    assert!(
        gid_cache_size() > initial_size,
        "GID cache should grow after lookup"
    );
}

#[cfg(unix)]
#[test]
fn numeric_ids_bypasses_cache() {
    let _lock = cache_lock().lock().unwrap();
    clear_id_caches();
    let initial_uid_size = uid_cache_size();
    let initial_gid_size = gid_cache_size();

    let _ = map_uid(1000, true);
    let _ = map_gid(1000, true);

    assert_eq!(
        uid_cache_size(),
        initial_uid_size,
        "UID cache should not change for numeric lookups"
    );
    assert_eq!(
        gid_cache_size(),
        initial_gid_size,
        "GID cache should not change for numeric lookups"
    );
}

#[cfg(unix)]
#[test]
fn repeated_lookups_return_same_result() {
    let _lock = cache_lock().lock().unwrap();
    clear_id_caches();

    let first = map_uid(1000, false);
    let second = map_uid(1000, false);
    let third = map_uid(1000, false);

    assert_eq!(first, second);
    assert_eq!(second, third);
}

#[cfg(unix)]
#[test]
fn clear_id_caches_empties_both_caches() {
    let _lock = cache_lock().lock().unwrap();
    let _ = map_uid(1000, false);
    let _ = map_gid(1000, false);

    clear_id_caches();

    assert_eq!(uid_cache_size(), 0, "UID cache should be empty after clear");
    assert_eq!(gid_cache_size(), 0, "GID cache should be empty after clear");
}

// upstream invariant: "The special uid 0 and the special group 0 are never
// mapped via user/group names even if the --numeric-ids option is not specified."

#[cfg(unix)]
#[test]
fn map_uid_zero_bypasses_name_lookup_even_without_numeric_ids() {
    let result = map_uid(0, false);
    assert!(result.is_some());
    assert_eq!(result.unwrap().as_raw(), 0);
}

#[cfg(unix)]
#[test]
fn map_gid_zero_bypasses_name_lookup_even_without_numeric_ids() {
    let result = map_gid(0, false);
    assert!(result.is_some());
    assert_eq!(result.unwrap().as_raw(), 0);
}

#[cfg(unix)]
#[test]
fn map_uid_zero_does_not_populate_cache() {
    let _lock = cache_lock().lock().unwrap();
    clear_id_caches();

    let _ = map_uid(0, false);

    assert_eq!(
        uid_cache_size(),
        0,
        "UID 0 should bypass cache entirely, not populate it"
    );
}

#[cfg(unix)]
#[test]
fn map_gid_zero_does_not_populate_cache() {
    let _lock = cache_lock().lock().unwrap();
    clear_id_caches();

    let _ = map_gid(0, false);

    assert_eq!(
        gid_cache_size(),
        0,
        "GID 0 should bypass cache entirely, not populate it"
    );
}

#[cfg(unix)]
#[test]
fn map_uid_zero_with_numeric_ids_true() {
    let result = map_uid(0, true);
    assert!(result.is_some());
    assert_eq!(result.unwrap().as_raw(), 0);
}

#[cfg(unix)]
#[test]
fn map_gid_zero_with_numeric_ids_true() {
    let result = map_gid(0, true);
    assert!(result.is_some());
    assert_eq!(result.unwrap().as_raw(), 0);
}

#[cfg(unix)]
#[test]
fn non_zero_ids_still_use_name_lookup_when_numeric_ids_false() {
    let _lock = cache_lock().lock().unwrap();
    clear_id_caches();

    let _ = map_uid(1000, false);
    assert!(
        uid_cache_size() > 0,
        "Non-zero UID should populate cache via name lookup path"
    );

    clear_id_caches();

    let _ = map_gid(1000, false);
    assert!(
        gid_cache_size() > 0,
        "Non-zero GID should populate cache via name lookup path"
    );
}

#[cfg(not(unix))]
#[test]
fn non_unix_map_uid_returns_raw_value() {
    assert_eq!(map_uid(0, false), Some(0));
    assert_eq!(map_uid(1000, false), Some(1000));
    assert_eq!(map_uid(65534, true), Some(65534));
}

#[cfg(not(unix))]
#[test]
fn non_unix_map_gid_returns_raw_value() {
    assert_eq!(map_gid(0, false), Some(0));
    assert_eq!(map_gid(1000, false), Some(1000));
    assert_eq!(map_gid(65534, true), Some(65534));
}

#[cfg(not(unix))]
#[test]
fn non_unix_map_uid_numeric_ids_flag_ignored() {
    // On non-unix, numeric_ids flag has no effect - always passthrough.
    assert_eq!(map_uid(42, true), map_uid(42, false));
}

#[cfg(not(unix))]
#[test]
fn non_unix_map_gid_numeric_ids_flag_ignored() {
    assert_eq!(map_gid(42, true), map_gid(42, false));
}

// Process-wide name memo (name_cache): each distinct id must trigger at most one
// underlying NSS lookup, mirroring upstream add_uid()/add_gid().

#[test]
fn cached_user_name_looks_up_once_per_distinct_id() {
    let _lock = name_cache_lock().lock().unwrap();
    clear_name_caches();
    reset_nss_lookup_count();

    let first = lookup_user_name_cached(0).unwrap();
    for _ in 0..8 {
        let repeat = lookup_user_name_cached(0).unwrap();
        assert_eq!(repeat, first, "cached name must be byte-for-byte identical");
    }

    assert_eq!(
        nss_lookup_count(),
        1,
        "a distinct uid must hit NSS at most once"
    );
}

#[test]
fn cached_group_name_looks_up_once_per_distinct_id() {
    let _lock = name_cache_lock().lock().unwrap();
    clear_name_caches();
    reset_nss_lookup_count();

    let first = lookup_group_name_cached(0).unwrap();
    for _ in 0..8 {
        let repeat = lookup_group_name_cached(0).unwrap();
        assert_eq!(repeat, first, "cached name must be byte-for-byte identical");
    }

    assert_eq!(
        nss_lookup_count(),
        1,
        "a distinct gid must hit NSS at most once"
    );
}

#[test]
fn cached_user_name_matches_uncached_bytes() {
    let _lock = name_cache_lock().lock().unwrap();
    clear_name_caches();

    let uncached = lookup_user_name(0).unwrap();
    let cached = lookup_user_name_cached(0).unwrap();
    assert_eq!(cached, uncached);
}

#[test]
fn cached_group_name_matches_uncached_bytes() {
    let _lock = name_cache_lock().lock().unwrap();
    clear_name_caches();

    let uncached = lookup_group_name(0).unwrap();
    let cached = lookup_group_name_cached(0).unwrap();
    assert_eq!(cached, uncached);
}

#[test]
fn cached_lookup_memoizes_missing_id() {
    let _lock = name_cache_lock().lock().unwrap();
    clear_name_caches();
    reset_nss_lookup_count();

    // A non-existent id resolves to None; the None outcome must be cached too so
    // repeated misses do not re-hit NSS.
    let first = lookup_user_name_cached(999_999_999).unwrap();
    let second = lookup_user_name_cached(999_999_999).unwrap();
    assert_eq!(first, second);
    assert_eq!(
        nss_lookup_count(),
        1,
        "a cached None must not re-trigger NSS lookups"
    );
}

#[test]
fn cached_lookup_distinct_ids_each_look_up() {
    let _lock = name_cache_lock().lock().unwrap();
    clear_name_caches();
    reset_nss_lookup_count();

    let _ = lookup_user_name_cached(0).unwrap();
    let _ = lookup_user_name_cached(999_999_998).unwrap();
    assert_eq!(
        nss_lookup_count(),
        2,
        "two distinct ids must each trigger exactly one NSS lookup"
    );
}

struct FixedConverter;

impl NameConverterCallbacks for FixedConverter {
    fn uid_to_name(&mut self, _uid: u32) -> ConverterOutcome<String> {
        ConverterOutcome::Resolved("converted-user".to_string())
    }
    fn gid_to_name(&mut self, _gid: u32) -> ConverterOutcome<String> {
        ConverterOutcome::Resolved("converted-group".to_string())
    }
    fn name_to_uid(&mut self, _name: &str) -> ConverterOutcome<u32> {
        ConverterOutcome::Unknown
    }
    fn name_to_gid(&mut self, _name: &str) -> ConverterOutcome<u32> {
        ConverterOutcome::Unknown
    }
}

#[test]
fn cached_lookup_bypasses_cache_when_converter_installed() {
    let _lock = name_cache_lock().lock().unwrap();
    clear_name_caches();
    reset_nss_lookup_count();

    set_name_converter(Box::new(FixedConverter));

    // The converter's per-thread result must win and must not consult or
    // populate the process-wide memo.
    let user = lookup_user_name_cached(4242).unwrap();
    assert_eq!(user, Some(b"converted-user".to_vec()));
    let group = lookup_group_name_cached(4242).unwrap();
    assert_eq!(group, Some(b"converted-group".to_vec()));

    assert_eq!(
        nss_lookup_count(),
        0,
        "converter path must not touch the memo miss counter"
    );

    clear_name_converter();
}

/// A converter that answers exactly one name and disclaims everything else.
///
/// The disclaimed answers are the point: they are what a real converter emits
/// for a name outside the operator's mapping, and what a dead converter's
/// caller sees.
///
/// Scoped to Unix because both its consumers are: the host-database contrast
/// they assert only exists where there IS a host database to leak from.
#[cfg(unix)]
struct DisclaimingConverter;

#[cfg(unix)]
impl NameConverterCallbacks for DisclaimingConverter {
    fn uid_to_name(&mut self, _uid: u32) -> ConverterOutcome<String> {
        ConverterOutcome::Unknown
    }

    fn gid_to_name(&mut self, _gid: u32) -> ConverterOutcome<String> {
        ConverterOutcome::Unknown
    }

    fn name_to_uid(&mut self, name: &str) -> ConverterOutcome<u32> {
        (name == "mapped-only").then_some(4242).into()
    }

    fn name_to_gid(&mut self, name: &str) -> ConverterOutcome<u32> {
        (name == "mapped-only").then_some(4243).into()
    }
}

/// A converter whose request/answer stream has broken.
///
/// upstream: clientserver.c:1329-1334 - a converter that cannot be written to
/// ends the session; it never reports a name as merely absent.
#[cfg(unix)]
struct DeadConverter;

#[cfg(unix)]
impl NameConverterCallbacks for DeadConverter {
    fn uid_to_name(&mut self, _uid: u32) -> ConverterOutcome<String> {
        ConverterOutcome::Failed(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
    }

    fn gid_to_name(&mut self, _gid: u32) -> ConverterOutcome<String> {
        ConverterOutcome::Failed(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
    }

    fn name_to_uid(&mut self, _name: &str) -> ConverterOutcome<u32> {
        ConverterOutcome::Failed(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
    }

    fn name_to_gid(&mut self, _name: &str) -> ConverterOutcome<u32> {
        ConverterOutcome::Failed(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
    }
}

/// A dead converter must be distinguishable from a name it does not know.
///
/// The contrast with [`DisclaimingConverter`] is the whole point: that one
/// answers "no such name" and this one cannot answer at all, and before the
/// outcome type existed both arrived at the call site as the same `None`. The
/// host database is left in place as the second discriminator - a fall-through
/// would resolve `root`, and the failure must not become that either.
#[cfg(unix)]
#[test]
fn a_converter_that_cannot_answer_is_not_a_name_that_does_not_resolve() {
    clear_name_converter();
    assert!(
        lookup_user_by_name(b"root").expect("host lookup").is_some(),
        "this host cannot resolve `root`, so this test cannot discriminate"
    );

    set_name_converter(Box::new(DisclaimingConverter));
    assert_eq!(
        lookup_user_by_name(b"root").expect("a disclaimed name is not an error"),
        None,
        "a converter that answers `unknown` yields `None`"
    );

    set_name_converter(Box::new(DeadConverter));
    for outcome in [
        lookup_user_by_name(b"root").map(|id| id.is_some()),
        lookup_group_by_name(b"root").map(|id| id.is_some()),
    ] {
        let err = outcome.expect_err("a dead converter must not read as `no such name`");
        assert!(err.is_converter_failure(), "{err}");
    }
    assert!(
        lookup_user_name(0)
            .expect_err("a dead converter must not read as `this uid has no name`")
            .is_converter_failure()
    );
    assert!(
        lookup_group_name(0)
            .expect_err("a dead converter must not read as `this gid has no name`")
            .is_converter_failure()
    );

    // A host-database failure keeps its tolerance; only the converter's is
    // fatal. Without this contrast the helper could simply propagate
    // everything and still pass the assertions above.
    assert!(
        no_id_unless_converter_failed(lookup_user_by_name(b"root")).is_err(),
        "a converter failure survives the database-failure tolerance"
    );

    clear_name_converter();
}

/// An installed converter REPLACES the host user database; it is not merely
/// consulted ahead of it.
///
/// upstream: uidlist.c:114-121, :131-138, :154-163, :180-189 - each lookup is
/// `if (namecvt_pid) { namecvt_call(...) } else { getpw*()/getgr*() }`. The
/// directive exists to isolate a session from the host database, so answering
/// "unknown" must mean unknown, not "now go ask /etc/passwd".
///
/// The probe name is deliberately one the host DOES resolve. A bogus name such
/// as `no_such_user_zzz` returns `None` whether or not the fall-through exists,
/// so a test using one would pass with the bug intact.
#[cfg(unix)]
#[test]
fn installed_converter_replaces_the_host_database() {
    clear_name_converter();
    let host_uid = lookup_user_by_name(b"root").expect("host lookup");
    // Non-vacuity: `root` really is resolvable without a converter, so a `None`
    // below is the converter's verdict and not simply an absent user.
    assert!(
        host_uid.is_some(),
        "this host cannot resolve `root`, so the fall-through would be \
         unobservable and this test cannot discriminate"
    );

    set_name_converter(Box::new(DisclaimingConverter));

    // Non-vacuity: the converter is genuinely installed and genuinely answering.
    assert_eq!(
        lookup_user_by_name(b"mapped-only").expect("lookup"),
        Some(4242),
        "the converter is not installed, so the assertions below prove nothing"
    );

    assert_eq!(
        lookup_user_by_name(b"root").expect("lookup"),
        None,
        "the converter disclaimed `root`, so the lookup must fail - falling \
         through to the host database would return {host_uid:?} and defeat the \
         isolation the directive exists for"
    );
    assert_eq!(
        lookup_user_name(0).expect("lookup"),
        None,
        "the converter disclaimed uid 0; the host name must not leak through"
    );

    clear_name_converter();
}

/// upstream: uidlist.c:146-147 `user_to_uid()` / :172-173 `group_to_gid()` -
/// `if (!name || !*name) return 0;`. An empty name is decided before either the
/// converter or the host database is consulted, so no request is framed for it.
///
/// The converter is one that FAILS every query it is given. A converter that
/// merely answered "unknown" would return `None` for the empty name whether or
/// not the guard exists, which is how a missing guard passes a test: the
/// discriminator is that reaching the converter at all must be observable.
#[cfg(unix)]
#[test]
fn an_empty_name_resolves_to_nothing_without_consulting_the_converter() {
    set_name_converter(Box::new(DeadConverter));

    assert_eq!(
        lookup_user_by_name(b"").expect("an empty name is decided before the converter"),
        None
    );
    assert_eq!(
        lookup_group_by_name(b"").expect("an empty name is decided before the converter"),
        None
    );

    // Non-vacuity: this converter IS installed and IS reached by a non-empty
    // name, so the two `Ok(None)`s above are the empty-name rule and not an
    // inert fixture.
    assert!(lookup_user_by_name(b"anything").is_err());
    assert!(lookup_group_by_name(b"anything").is_err());

    clear_name_converter();
}
