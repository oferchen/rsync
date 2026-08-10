//! Xattr name prefix translation for cross-platform compatibility.
//!
//! Different operating systems have different xattr namespace conventions:
//!
//! - **Linux**: Supports multiple namespaces (`user.`, `system.`, `security.`, `trusted.`)
//! - **macOS/BSD**: Only supports `user.` namespace (others are system-internal)
//!
//! # Wire Format
//!
//! Upstream rsync transmits xattr names **byte-for-byte** as they appear in
//! the platform's `listxattr(2)` output (on Linux that includes the
//! `user.` namespace prefix). The receiver consults `am_root` to decide
//! whether unprefixed wire names should be folded into the disguised
//! `user.rsync.*` hierarchy or dropped. Non-Linux peers use the `rsync.`
//! prefix as the disguise namespace.
//!
//! Reserved internal attributes (`user.rsync.%suffix` on Linux,
//! `rsync.%suffix` elsewhere) are never sent on the wire by the sender:
//! they belong to rsync's own metadata channel.
//!
//! # Upstream Reference
//!
//! - `xattrs.c` lines 254-258: `rsync_xal_get()` non-root namespace filter
//! - `xattrs.c` lines 494-542: `send_xattr()` writes names verbatim except
//!   in fake-super (`am_root < 0`), where the `user.rsync.` prefix is
//!   stripped from disguised entries
//! - `xattrs.c` lines 820-847: `receive_xattr()` name handling - Linux
//!   keeps `user.*` verbatim and disguises everything else under
//!   `user.rsync.`; non-Linux strips `user.` and disguises the rest

#[cfg(target_os = "linux")]
use super::USER_PREFIX;
use super::{RSYNC_PREFIX, SYSTEM_PREFIX};

/// Translates an xattr name from local format to wire format.
///
/// On every platform the local name is emitted verbatim once it has
/// passed the rsync-internal filter. This matches the upstream sender
/// (`xattrs.c:send_xattr()`), which writes the bytes returned by
/// `listxattr(2)` directly into the protocol stream. Upstream's only
/// transformation is the fake-super (`am_root < 0`) prefix strip; we do
/// not model that here because the sender currently runs with
/// `am_root = false` (see `transfer/generator/file_list/entry.rs`).
///
/// Namespace filtering for non-root senders is performed earlier in
/// `metadata::xattr::list_attributes` via `is_xattr_permitted`; this
/// function additionally drops `system.*` for non-root callers as a
/// defensive belt-and-braces check so direct callers cannot leak a
/// namespace they could never set.
///
/// # Arguments
///
/// * `name` - Local xattr name
/// * `am_root` - Whether the sender has root privileges (gates `system.*`)
///
/// # Returns
///
/// The wire-format name, or `None` if this xattr should be skipped.
pub fn local_to_wire(name: &[u8], am_root: bool) -> Option<Vec<u8>> {
    let name_str = match std::str::from_utf8(name) {
        Ok(s) => s,
        // Non-UTF8 names cannot be rsync-internal markers, so pass them
        // through verbatim.
        Err(_) => return Some(name.to_vec()),
    };

    // upstream: xattrs.c:261-267 - rsync.%FOO internals are never sent.
    if is_rsync_internal(name_str) {
        return None;
    }

    // upstream: xattrs.c:256 - non-root never exposes the system namespace.
    if name_str.starts_with(SYSTEM_PREFIX) && !am_root {
        return None;
    }

    // upstream: xattrs.c:524-532 - on Linux the name is written verbatim;
    // on non-Linux the user. prefix is added by send_xattr before the
    // bytes hit the wire. Mirror that here so peers see identical bytes.
    #[cfg(target_os = "linux")]
    {
        Some(name.to_vec())
    }

    #[cfg(not(target_os = "linux"))]
    {
        // upstream: xattrs.c:518-530 - non-Linux senders insert USER_PREFIX
        // for names that are not already disguised under RSYNC_PREFIX.
        if name_str.starts_with(RSYNC_PREFIX) {
            Some(name.to_vec())
        } else {
            let mut wire_name = Vec::with_capacity(USER_PREFIX_NON_LINUX.len() + name.len());
            wire_name.extend_from_slice(USER_PREFIX_NON_LINUX.as_bytes());
            wire_name.extend_from_slice(name);
            Some(wire_name)
        }
    }
}

/// User namespace prefix added by the non-Linux sender path.
///
/// Non-Linux platforms (macOS, BSD) only expose a single namespace, so
/// the wire convention adds `user.` to every name that is not already
/// disguised under [`RSYNC_PREFIX`]. Mirrors upstream
/// `xattrs.c:518-530`.
#[cfg(not(target_os = "linux"))]
const USER_PREFIX_NON_LINUX: &str = "user.";

/// Translates an xattr name from wire format to local format.
///
/// Mirrors upstream `xattrs.c:receive_xattr()` (lines 820-847):
///
/// # Linux Behavior
///
/// - `user.foo` -> `user.foo` (already in user namespace; keep verbatim)
/// - `user.rsync.%stat` -> `user.rsync.%stat` (rsync internal, keep verbatim)
/// - `system.foo` (root) -> `system.foo` (root can write the original
///   namespace verbatim)
/// - `system.foo` (non-root) -> `user.rsync.system.foo` (disguised so the
///   non-user namespace survives under the user hierarchy)
///
/// # Non-Linux Behavior
///
/// - `user.foo` -> `foo` (strip the user namespace prefix since the OS
///   has a flat namespace)
/// - `system.foo` (root) -> `rsync.system.foo` (disguised; root can still
///   write the rsync hierarchy)
/// - everything else (non-root) -> dropped (`None`) - the disguised slot
///   only exists for root to satisfy upstream's interop expectations
///
/// # Arguments
///
/// * `wire_name` - Wire-format xattr name (verbatim bytes from the wire)
/// * `am_root` - Whether the receiver has root privileges
///
/// # Returns
///
/// The local-format name, or `None` if this xattr cannot be stored
/// locally (matches upstream's `free(ptr); continue` skip).
pub fn wire_to_local(wire_name: &[u8], am_root: bool) -> Option<Vec<u8>> {
    #[cfg(target_os = "linux")]
    {
        // upstream: xattrs.c:820-831 - keep user.* verbatim; non-user
        // names are disguised under user.rsync. for non-root receivers.
        // Root receivers store names verbatim into their original
        // namespace (system., security., trusted., etc.).
        if wire_name.starts_with(USER_PREFIX.as_bytes()) {
            return Some(wire_name.to_vec());
        }
        if am_root {
            return Some(wire_name.to_vec());
        }
        // Non-root receiver: disguise the non-user-namespace name under
        // user.rsync.<wire_name>. Upstream additionally honours
        // saw_xattr_filter to drop the entry entirely; we always keep it
        // because the filter state is not plumbed through here yet.
        let mut local = Vec::with_capacity(RSYNC_PREFIX.len() + wire_name.len());
        local.extend_from_slice(RSYNC_PREFIX.as_bytes());
        local.extend_from_slice(wire_name);
        Some(local)
    }

    #[cfg(not(target_os = "linux"))]
    {
        let wire_str = match std::str::from_utf8(wire_name) {
            Ok(s) => s,
            Err(_) => return Some(wire_name.to_vec()),
        };

        // upstream: xattrs.c:836-846 - strip the user. prefix that the
        // sender added on the wire so the name slots back into the flat
        // namespace this OS exposes.
        if let Some(stripped) = wire_str.strip_prefix("user.") {
            return Some(stripped.as_bytes().to_vec());
        }

        // upstream: xattrs.c:839-845 - non-root receivers drop entries
        // they could not store. Root receivers disguise them under
        // rsync.<wire_name>.
        if !am_root {
            return None;
        }
        let mut local = Vec::with_capacity(RSYNC_PREFIX.len() + wire_name.len());
        local.extend_from_slice(RSYNC_PREFIX.as_bytes());
        local.extend_from_slice(wire_name);
        Some(local)
    }
}

/// Checks if an xattr name is an rsync internal attribute.
///
/// Rsync internal attributes use the pattern `rsync.%suffix` or `user.rsync.%suffix`.
/// These are used for storing metadata like stat info and ACLs (the fake-super
/// `%stat`/`%aacl`/`%dacl` channel). Upstream never transfers them as -X data:
/// the sender skips them (`xattrs.c:261-267`, `am_sender && preserve_xattrs < 2`),
/// so a local copy must exclude them from both the copy and the delete pass.
pub fn is_rsync_internal(name: &str) -> bool {
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
        fn local_to_wire_keeps_user_prefix_verbatim() {
            // upstream: xattrs.c:524-532 - Linux sender writes the local
            // name byte-for-byte. Stripping `user.` here would diverge
            // from upstream and cause receivers to land the entry in the
            // wrong namespace (BR-3h, issue #2494).
            let result = local_to_wire(b"user.foo", false);
            assert_eq!(result, Some(b"user.foo".to_vec()));
        }

        #[test]
        fn local_to_wire_keeps_rsync_disguise_verbatim() {
            // Non-internal user.rsync.* names are passed verbatim. The
            // fake-super (`am_root < 0`) strip is not modelled here
            // because the live sender always runs with am_root == false.
            let result = local_to_wire(b"user.rsync.system.foo", false);
            assert_eq!(result, Some(b"user.rsync.system.foo".to_vec()));
        }

        #[test]
        fn local_to_wire_skips_internal() {
            let result = local_to_wire(b"user.rsync.%stat", false);
            assert_eq!(result, None);
        }

        #[test]
        fn local_to_wire_system_needs_root() {
            assert_eq!(local_to_wire(b"system.foo", false), None);
            assert_eq!(
                local_to_wire(b"system.foo", true),
                Some(b"system.foo".to_vec()),
            );
        }

        #[test]
        fn local_to_wire_other_namespaces_pass_through_for_root() {
            assert_eq!(
                local_to_wire(b"security.selinux", true),
                Some(b"security.selinux".to_vec()),
            );
            assert_eq!(
                local_to_wire(b"trusted.foo", true),
                Some(b"trusted.foo".to_vec()),
            );
        }

        #[test]
        fn wire_to_local_keeps_user_prefix_verbatim() {
            // upstream: xattrs.c:820-823 - names already inside the user
            // namespace are kept byte-for-byte. The previous behavior of
            // prepending an additional `user.` produced `user.user.foo`
            // (BR-3h, issue #2494).
            let result = wire_to_local(b"user.foo", false);
            assert_eq!(result, Some(b"user.foo".to_vec()));
        }

        #[test]
        fn wire_to_local_keeps_user_rsync_internal_verbatim() {
            let result = wire_to_local(b"user.rsync.%stat", false);
            assert_eq!(result, Some(b"user.rsync.%stat".to_vec()));
        }

        #[test]
        fn wire_to_local_disguises_non_user_for_non_root() {
            // upstream: xattrs.c:827-829 - non-root receivers prepend
            // RSYNC_PREFIX (`user.rsync.`) to non-user-namespace wire
            // names so the entry survives in the user hierarchy.
            let result = wire_to_local(b"system.foo", false);
            assert_eq!(result, Some(b"user.rsync.system.foo".to_vec()));
        }

        #[test]
        fn wire_to_local_keeps_non_user_verbatim_for_root() {
            // upstream: xattrs.c:820-831 - root receivers store
            // non-user-namespace names directly into their original
            // namespace.
            assert_eq!(
                wire_to_local(b"system.foo", true),
                Some(b"system.foo".to_vec()),
            );
            assert_eq!(
                wire_to_local(b"security.selinux", true),
                Some(b"security.selinux".to_vec()),
            );
        }
    }

    #[cfg(not(target_os = "linux"))]
    mod non_linux_tests {
        use super::*;

        #[test]
        fn local_to_wire_adds_user_prefix() {
            // upstream: xattrs.c:518-530 - non-Linux senders insert
            // USER_PREFIX so the wire bytes match what a Linux peer
            // would produce.
            let result = local_to_wire(b"foo", false);
            assert_eq!(result, Some(b"user.foo".to_vec()));
        }

        #[test]
        fn local_to_wire_keeps_disguised_rsync_namespace() {
            let result = local_to_wire(b"rsync.system.foo", true);
            assert_eq!(result, Some(b"rsync.system.foo".to_vec()));
        }

        #[test]
        fn wire_to_local_strips_user_prefix() {
            // upstream: xattrs.c:836-838 - non-Linux receivers strip the
            // user. prefix sent over the wire to obtain a flat-namespace
            // local name.
            let result = wire_to_local(b"user.foo", false);
            assert_eq!(result, Some(b"foo".to_vec()));
        }
    }
}
