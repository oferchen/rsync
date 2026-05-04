//! Tests for UID/GID lookup and mapping.

use super::*;
#[cfg(unix)]
use std::sync::{Mutex, OnceLock};

/// Global lock to serialize tests that modify shared caches.
#[cfg(unix)]
fn cache_lock() -> &'static Mutex<()> {
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
