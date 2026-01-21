//! Xattr name prefix translation for cross-platform compatibility.
//!
//! Different operating systems have different xattr namespace conventions:
//!
//! - **Linux**: Supports multiple namespaces (`user.`, `system.`, `security.`, `trusted.`)
//! - **macOS/BSD**: Only supports `user.` namespace (others are system-internal)
//!
//! To enable cross-platform rsync transfers, xattr names are translated on the wire:
//!
//! # Wire Format
//!
//! On the wire, xattr names follow these conventions:
//!
//! - `user.*` namespaced attrs are sent without the `user.` prefix
//! - Non-user namespaced attrs (when sender is root) are disguised under `rsync.`
//! - The `rsync.%` prefix is reserved for rsync's internal attributes (stat, ACLs)
//!
//! # Upstream Reference
//!
//! - `xattrs.c` lines 509-528: `send_xattr_name()` logic
//! - `xattrs.c` lines 821-850: `receive_xattr()` name handling

#[cfg(target_os = "linux")]
use super::USER_PREFIX;
use super::{RSYNC_PREFIX, SYSTEM_PREFIX};

/// Translates an xattr name from local format to wire format.
///
/// # Linux Behavior
///
/// - `user.foo` -> `foo` (strip user prefix for wire)
/// - `system.foo` -> `rsync.system.foo` (disguise under rsync when root sends)
/// - `security.foo` -> `rsync.security.foo` (disguise under rsync when root sends)
/// - `user.rsync.%stat` -> `rsync.%stat` (rsync internal attrs pass through)
///
/// # Non-Linux Behavior
///
/// - `foo` -> `foo` (no user prefix to strip)
/// - Attrs are already in user-only namespace
///
/// # Arguments
///
/// * `name` - Local xattr name
/// * `am_root` - Whether the sender has root privileges (can access non-user namespaces)
///
/// # Returns
///
/// The wire-format name, or `None` if this xattr should be skipped.
#[must_use]
pub fn local_to_wire(name: &[u8], am_root: bool) -> Option<Vec<u8>> {
    let name_str = match std::str::from_utf8(name) {
        Ok(s) => s,
        Err(_) => return Some(name.to_vec()), // Pass through non-UTF8 names
    };

    // Skip rsync internal attributes (rsync.% or user.rsync.%)
    if is_rsync_internal(name_str) {
        return None;
    }

    // Handle system namespace (root only)
    if name_str.starts_with(SYSTEM_PREFIX) {
        if !am_root {
            // Non-root cannot access system namespace
            return None;
        }
        // Disguise under rsync prefix for wire transfer
        let mut wire_name = RSYNC_PREFIX.as_bytes().to_vec();
        wire_name.extend_from_slice(name);
        return Some(wire_name);
    }

    #[cfg(target_os = "linux")]
    {
        // Linux: strip user. prefix for wire format
        if name_str.starts_with(USER_PREFIX) {
            return Some(name[USER_PREFIX.len()..].to_vec());
        }
        // Handle other namespaces (security., trusted.) - root only
        if !am_root {
            // Non-root can only access user namespace
            return None;
        }
        // Disguise under rsync prefix
        let mut wire_name = RSYNC_PREFIX.as_bytes().to_vec();
        wire_name.extend_from_slice(name);
        Some(wire_name)
    }

    #[cfg(not(target_os = "linux"))]
    {
        // Non-Linux: xattrs are already in user-only namespace
        // Send as-is (no user. prefix to strip on these systems)
        Some(name.to_vec())
    }
}

/// Translates an xattr name from wire format to local format.
///
/// # Linux Behavior
///
/// - `foo` -> `user.foo` (add user prefix)
/// - `rsync.system.foo` -> `system.foo` (restore original namespace when root)
/// - `rsync.%stat` -> `user.rsync.%stat` (rsync internal attrs)
///
/// # Non-Linux Behavior
///
/// - `foo` -> `foo` (already in user namespace)
/// - `rsync.bar` -> `user.rsync.bar` (disguised attrs go into user.rsync hierarchy)
///
/// # Arguments
///
/// * `wire_name` - Wire-format xattr name
/// * `am_root` - Whether the receiver has root privileges (can write non-user namespaces)
///
/// # Returns
///
/// The local-format name, or `None` if this xattr cannot be stored locally.
#[must_use]
pub fn wire_to_local(wire_name: &[u8], am_root: bool) -> Option<Vec<u8>> {
    let wire_str = match std::str::from_utf8(wire_name) {
        Ok(s) => s,
        Err(_) => {
            // Non-UTF8: add user prefix and return
            #[cfg(target_os = "linux")]
            {
                let mut local = USER_PREFIX.as_bytes().to_vec();
                local.extend_from_slice(wire_name);
                return Some(local);
            }
            #[cfg(not(target_os = "linux"))]
            {
                return Some(wire_name.to_vec());
            }
        }
    };

    // Handle rsync-prefixed names (disguised namespaces)
    if let Some(inner) = wire_str.strip_prefix(RSYNC_PREFIX) {
        // Check for rsync internal attributes (rsync.%foo)
        if inner.starts_with('%') {
            // Keep as rsync internal attribute
            #[cfg(target_os = "linux")]
            {
                // On Linux, these become user.rsync.%foo
                let mut local = USER_PREFIX.as_bytes().to_vec();
                local.extend_from_slice(wire_name);
                return Some(local);
            }
            #[cfg(not(target_os = "linux"))]
            {
                // On non-Linux, keep as rsync.%foo
                return Some(wire_name.to_vec());
            }
        }

        // Disguised namespace (e.g., rsync.system.foo -> system.foo)
        if inner.starts_with("system.")
            || inner.starts_with("security.")
            || inner.starts_with("trusted.")
        {
            if am_root {
                // Root can write to the original namespace
                return Some(inner.as_bytes().to_vec());
            } else {
                // Non-root: keep disguised under user.rsync
                #[cfg(target_os = "linux")]
                {
                    let mut local = USER_PREFIX.as_bytes().to_vec();
                    local.extend_from_slice(wire_name);
                    return Some(local);
                }
                #[cfg(not(target_os = "linux"))]
                {
                    return Some(wire_name.to_vec());
                }
            }
        }

        // Other rsync. prefixed attrs
        #[cfg(target_os = "linux")]
        {
            let mut local = USER_PREFIX.as_bytes().to_vec();
            local.extend_from_slice(wire_name);
            return Some(local);
        }
        #[cfg(not(target_os = "linux"))]
        {
            return Some(wire_name.to_vec());
        }
    }

    // Regular attribute name (no special prefix)
    #[cfg(target_os = "linux")]
    {
        // On Linux, add user. prefix
        let mut local = USER_PREFIX.as_bytes().to_vec();
        local.extend_from_slice(wire_name);
        Some(local)
    }

    #[cfg(not(target_os = "linux"))]
    {
        // On non-Linux, keep as-is
        Some(wire_name.to_vec())
    }
}

/// Checks if an xattr name is an rsync internal attribute.
///
/// Rsync internal attributes use the pattern `rsync.%suffix` or `user.rsync.%suffix`.
/// These are used for storing metadata like stat info and ACLs.
fn is_rsync_internal(name: &str) -> bool {
    // Check for user.rsync.% pattern (Linux)
    if let Some(suffix) = name.strip_prefix("user.rsync.") {
        return suffix.starts_with('%');
    }
    // Check for rsync.% pattern (non-Linux or wire format)
    if let Some(suffix) = name.strip_prefix("rsync.") {
        return suffix.starts_with('%');
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rsync_internal_detection() {
        assert!(is_rsync_internal("user.rsync.%stat"));
        assert!(is_rsync_internal("user.rsync.%aacl"));
        assert!(is_rsync_internal("rsync.%stat"));
        assert!(!is_rsync_internal("user.rsync.normal"));
        assert!(!is_rsync_internal("rsync.normal"));
        assert!(!is_rsync_internal("user.foo"));
    }

    #[cfg(target_os = "linux")]
    mod linux_tests {
        use super::*;

        #[test]
        fn local_to_wire_strips_user_prefix() {
            let result = local_to_wire(b"user.foo", false);
            assert_eq!(result, Some(b"foo".to_vec()));
        }

        #[test]
        fn local_to_wire_skips_internal() {
            let result = local_to_wire(b"user.rsync.%stat", false);
            assert_eq!(result, None);
        }

        #[test]
        fn local_to_wire_system_needs_root() {
            let result = local_to_wire(b"system.foo", false);
            assert_eq!(result, None);

            let result = local_to_wire(b"system.foo", true);
            assert!(result.is_some());
            let wire = result.unwrap();
            assert!(wire.starts_with(b"user.rsync.system."));
        }

        #[test]
        fn wire_to_local_adds_user_prefix() {
            let result = wire_to_local(b"foo", false);
            assert_eq!(result, Some(b"user.foo".to_vec()));
        }

        #[test]
        fn wire_to_local_restores_system_for_root() {
            let result = wire_to_local(b"user.rsync.system.foo", true);
            assert_eq!(result, Some(b"system.foo".to_vec()));
        }

        #[test]
        fn wire_to_local_keeps_system_disguised_for_nonroot() {
            let result = wire_to_local(b"user.rsync.system.foo", false);
            assert!(result.is_some());
            let local = result.unwrap();
            assert!(local.starts_with(b"user."));
        }
    }

    #[cfg(not(target_os = "linux"))]
    mod non_linux_tests {
        use super::*;

        #[test]
        fn local_to_wire_passes_through() {
            let result = local_to_wire(b"foo", false);
            assert_eq!(result, Some(b"foo".to_vec()));
        }

        #[test]
        fn wire_to_local_passes_through() {
            let result = wire_to_local(b"foo", false);
            assert_eq!(result, Some(b"foo".to_vec()));
        }
    }
}
