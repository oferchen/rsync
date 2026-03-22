//! Protocol version compatibility restrictions.
//!
//! Implements the feature-version checks from upstream `compat.c:641-709`.
//! After protocol setup completes, certain features require a minimum protocol
//! version. This module validates those constraints and returns appropriate
//! errors when features are incompatible with the negotiated protocol.
//!
//! # Upstream Reference
//!
//! - `compat.c:652-668` - Protocol < 30 restrictions (ACLs, xattrs, append mode)
//! - `compat.c:671-676` - Delete mode default selection
//! - `compat.c:678-709` - Protocol < 29 restrictions (fuzzy, inplace+basis, multi-basis, prune)

use std::io;

use protocol::ProtocolVersion;

/// Transfer flags relevant to protocol compatibility checks.
///
/// Groups the feature flags that `apply_protocol_restrictions` needs
/// to validate against the negotiated protocol version.
#[derive(Debug, Clone, Default)]
pub struct ProtocolRestrictionFlags {
    /// `--acls` / `-A` preservation.
    pub preserve_acls: bool,
    /// `--xattrs` / `-X` preservation.
    pub preserve_xattrs: bool,
    /// `--fuzzy` / `-y` fuzzy matching.
    pub fuzzy_basis: bool,
    /// Number of `--compare-dest` / `--copy-dest` / `--link-dest` directories.
    pub basis_dir_count: usize,
    /// `--inplace` writing.
    pub inplace: bool,
    /// `--prune-empty-dirs` / `-m`.
    pub prune_empty_dirs: bool,
    /// Whether this is a local server (no remote connection).
    pub local_server: bool,
    /// Current append mode value (1 = auto, 2 = forced).
    pub append_mode: u8,
    /// `--delete` is enabled.
    pub delete_mode: bool,
    /// `--delete-before` explicitly set.
    pub delete_before: bool,
    /// `--delete-during` explicitly set.
    pub delete_during: bool,
    /// `--delete-after` explicitly set.
    pub delete_after: bool,
}

/// Result of applying protocol restrictions.
///
/// Contains any adjustments that the caller must apply to its configuration
/// after restriction checks pass.
#[derive(Debug, Clone, Default)]
pub struct RestrictionAdjustments {
    /// Adjusted append mode (upstream forces mode 1 -> 2 for protocol < 30).
    pub append_mode: Option<u8>,
    /// Adjusted delete mode: which phase to delete in.
    /// `Some(true)` = delete_before, `Some(false)` = delete_during.
    pub delete_before: Option<bool>,
}

/// Validates feature compatibility with the negotiated protocol version.
///
/// Mirrors upstream `compat.c:641-709`. Returns an error if any enabled
/// feature is incompatible with the negotiated protocol. On success, returns
/// adjustments the caller should apply.
///
/// # Errors
///
/// Returns `io::Error` with `InvalidInput` kind and a user-facing message
/// matching upstream rsync's error format when an incompatible feature is
/// detected.
///
/// # Upstream Reference
///
/// - `compat.c:652-668` - Protocol < 30 checks
/// - `compat.c:671-676` - Delete mode defaults
/// - `compat.c:678-709` - Protocol < 29 checks
#[must_use = "caller must apply returned adjustments"]
pub fn apply_protocol_restrictions(
    protocol: ProtocolVersion,
    flags: &ProtocolRestrictionFlags,
) -> Result<RestrictionAdjustments, io::Error> {
    let version: u32 = protocol.into();
    let mut adjustments = RestrictionAdjustments::default();

    // upstream compat.c:652-668 - protocol < 30 restrictions
    if version < 30 {
        // upstream compat.c:653-654 - append_mode 1 -> 2
        if flags.append_mode == 1 {
            adjustments.append_mode = Some(2);
        }

        // upstream compat.c:655-661 - ACLs require protocol 30+
        if flags.preserve_acls && !flags.local_server {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "--acls requires protocol 30 or higher (negotiated {version})."
                ),
            ));
        }

        // upstream compat.c:662-668 - xattrs require protocol 30+
        if flags.preserve_xattrs && !flags.local_server {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "--xattrs requires protocol 30 or higher (negotiated {version})."
                ),
            ));
        }
    }

    // upstream compat.c:671-676 - default delete phase selection
    if flags.delete_mode && !flags.delete_before && !flags.delete_during && !flags.delete_after {
        if version < 30 {
            adjustments.delete_before = Some(true);
        } else {
            adjustments.delete_before = Some(false);
        }
    }

    // upstream compat.c:678-709 - protocol < 29 restrictions
    if version < 29 {
        // upstream compat.c:679-685 - fuzzy requires 29+
        if flags.fuzzy_basis {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "--fuzzy requires protocol 29 or higher (negotiated {version})."
                ),
            ));
        }

        // upstream compat.c:687-693 - basis_dir + inplace requires 29+
        if flags.basis_dir_count > 0 && flags.inplace {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "--compare-dest/--copy-dest/--link-dest with --inplace requires \
                     protocol 29 or higher (negotiated {version})."
                ),
            ));
        }

        // upstream compat.c:695-701 - multiple basis dirs require 29+
        if flags.basis_dir_count > 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Using more than one --compare-dest/--copy-dest/--link-dest option \
                     requires protocol 29 or higher (negotiated {version})."
                ),
            ));
        }

        // upstream compat.c:703-709 - prune-empty-dirs requires 29+
        if flags.prune_empty_dirs {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "--prune-empty-dirs requires protocol 29 or higher (negotiated {version})."
                ),
            ));
        }
    }

    Ok(adjustments)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proto(version: u32) -> ProtocolVersion {
        ProtocolVersion::try_from(version).unwrap()
    }

    #[test]
    fn protocol_32_no_restrictions() {
        let flags = ProtocolRestrictionFlags {
            preserve_acls: true,
            preserve_xattrs: true,
            fuzzy_basis: true,
            basis_dir_count: 3,
            inplace: true,
            prune_empty_dirs: true,
            ..Default::default()
        };
        let result = apply_protocol_restrictions(proto(32), &flags);
        assert!(result.is_ok());
    }

    #[test]
    fn protocol_30_allows_acls_and_xattrs() {
        let flags = ProtocolRestrictionFlags {
            preserve_acls: true,
            preserve_xattrs: true,
            ..Default::default()
        };
        assert!(apply_protocol_restrictions(proto(30), &flags).is_ok());
    }

    #[test]
    fn protocol_29_rejects_acls() {
        let flags = ProtocolRestrictionFlags {
            preserve_acls: true,
            ..Default::default()
        };
        let err = apply_protocol_restrictions(proto(29), &flags).unwrap_err();
        assert!(err.to_string().contains("--acls requires protocol 30"));
    }

    #[test]
    fn protocol_29_rejects_xattrs() {
        let flags = ProtocolRestrictionFlags {
            preserve_xattrs: true,
            ..Default::default()
        };
        let err = apply_protocol_restrictions(proto(29), &flags).unwrap_err();
        assert!(err.to_string().contains("--xattrs requires protocol 30"));
    }

    #[test]
    fn protocol_29_allows_acls_for_local_server() {
        let flags = ProtocolRestrictionFlags {
            preserve_acls: true,
            local_server: true,
            ..Default::default()
        };
        assert!(apply_protocol_restrictions(proto(29), &flags).is_ok());
    }

    #[test]
    fn protocol_28_rejects_fuzzy() {
        let flags = ProtocolRestrictionFlags {
            fuzzy_basis: true,
            ..Default::default()
        };
        let err = apply_protocol_restrictions(proto(28), &flags).unwrap_err();
        assert!(err.to_string().contains("--fuzzy requires protocol 29"));
    }

    #[test]
    fn protocol_28_rejects_basis_dir_with_inplace() {
        let flags = ProtocolRestrictionFlags {
            basis_dir_count: 1,
            inplace: true,
            ..Default::default()
        };
        let err = apply_protocol_restrictions(proto(28), &flags).unwrap_err();
        assert!(err.to_string().contains("--inplace requires protocol 29"));
    }

    #[test]
    fn protocol_28_rejects_multiple_basis_dirs() {
        let flags = ProtocolRestrictionFlags {
            basis_dir_count: 2,
            ..Default::default()
        };
        let err = apply_protocol_restrictions(proto(28), &flags).unwrap_err();
        assert!(err.to_string().contains("more than one"));
    }

    #[test]
    fn protocol_28_rejects_prune_empty_dirs() {
        let flags = ProtocolRestrictionFlags {
            prune_empty_dirs: true,
            ..Default::default()
        };
        let err = apply_protocol_restrictions(proto(28), &flags).unwrap_err();
        assert!(err.to_string().contains("--prune-empty-dirs requires protocol 29"));
    }

    #[test]
    fn protocol_29_allows_fuzzy_and_prune() {
        let flags = ProtocolRestrictionFlags {
            fuzzy_basis: true,
            prune_empty_dirs: true,
            basis_dir_count: 3,
            inplace: true,
            ..Default::default()
        };
        assert!(apply_protocol_restrictions(proto(29), &flags).is_ok());
    }

    #[test]
    fn append_mode_1_becomes_2_below_protocol_30() {
        let flags = ProtocolRestrictionFlags {
            append_mode: 1,
            ..Default::default()
        };
        let adj = apply_protocol_restrictions(proto(29), &flags).unwrap();
        assert_eq!(adj.append_mode, Some(2));
    }

    #[test]
    fn append_mode_unchanged_at_protocol_30() {
        let flags = ProtocolRestrictionFlags {
            append_mode: 1,
            ..Default::default()
        };
        let adj = apply_protocol_restrictions(proto(30), &flags).unwrap();
        assert_eq!(adj.append_mode, None);
    }

    #[test]
    fn delete_defaults_to_delete_before_below_30() {
        let flags = ProtocolRestrictionFlags {
            delete_mode: true,
            ..Default::default()
        };
        let adj = apply_protocol_restrictions(proto(29), &flags).unwrap();
        assert_eq!(adj.delete_before, Some(true));
    }

    #[test]
    fn delete_defaults_to_delete_during_at_30_plus() {
        let flags = ProtocolRestrictionFlags {
            delete_mode: true,
            ..Default::default()
        };
        let adj = apply_protocol_restrictions(proto(30), &flags).unwrap();
        assert_eq!(adj.delete_before, Some(false));
    }

    #[test]
    fn explicit_delete_phase_not_overridden() {
        let flags = ProtocolRestrictionFlags {
            delete_mode: true,
            delete_after: true,
            ..Default::default()
        };
        let adj = apply_protocol_restrictions(proto(29), &flags).unwrap();
        assert_eq!(adj.delete_before, None);
    }

    #[test]
    fn protocol_28_single_basis_dir_without_inplace_ok() {
        let flags = ProtocolRestrictionFlags {
            basis_dir_count: 1,
            ..Default::default()
        };
        assert!(apply_protocol_restrictions(proto(28), &flags).is_ok());
    }
}
