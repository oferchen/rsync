//! Protocol version compatibility restrictions.
//!
//! Implements the feature-version checks from upstream `compat.c:641-709`.
//! After protocol setup completes, certain features require a minimum protocol
//! version. This module validates those constraints and returns appropriate
//! errors when features are incompatible with the negotiated protocol.
//!
//! # Upstream Reference
//!
//! - `compat.c:652-668` - Protocol < 30 restrictions (ACLs, xattrs)
//! - `compat.c:678-709` - Protocol < 29 restrictions (fuzzy, inplace+basis, multi-basis, prune)

use std::io;

use protocol::ProtocolVersion;

/// Transfer flags relevant to protocol compatibility checks.
///
/// Groups the feature flags that `refuse_unsupported_options` needs
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
}

/// Refuses options the negotiated protocol cannot carry.
///
/// Every peer runs these checks in `setup_protocol()` before any further
/// exchange, so each side refuses on its own instead of failing mid-transfer.
///
/// # Errors
///
/// A [`protocol::ProtocolViolation`]-tagged error carrying upstream's exact
/// text, so the exit-code mapper yields `RERR_PROTOCOL` (2).
///
/// # Upstream Reference
///
/// - `compat.c:663-676` - `--acls` / `--xattrs` require protocol 30
///   unless `local_server`
/// - `compat.c:684-713` - `--fuzzy`, a basis dir with `--inplace`, several
///   basis dirs and `--prune-empty-dirs` require protocol 29
pub fn refuse_unsupported_options(
    protocol: ProtocolVersion,
    flags: &ProtocolRestrictionFlags,
) -> io::Result<()> {
    let version: u32 = protocol.into();
    let refusal = if version < 30 && flags.preserve_acls && !flags.local_server {
        format!("--acls requires protocol 30 or higher (negotiated {version}).")
    } else if version < 30 && flags.preserve_xattrs && !flags.local_server {
        format!("--xattrs requires protocol 30 or higher (negotiated {version}).")
    } else if version >= 29 {
        return Ok(());
    } else if flags.fuzzy_basis {
        format!("--fuzzy requires protocol 29 or higher (negotiated {version}).")
    } else if flags.basis_dir_count > 0 && flags.inplace {
        format!(
            "--compare-dest/--copy-dest/--link-dest with --inplace requires \
             protocol 29 or higher (negotiated {version})."
        )
    } else if flags.basis_dir_count > 1 {
        format!(
            "Using more than one --compare-dest/--copy-dest/--link-dest option \
             requires protocol 29 or higher (negotiated {version})."
        )
    } else if flags.prune_empty_dirs {
        format!("--prune-empty-dirs requires protocol 29 or higher (negotiated {version}).")
    } else {
        return Ok(());
    };
    Err(protocol::protocol_violation(refusal))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proto(version: u8) -> ProtocolVersion {
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
        let result = refuse_unsupported_options(proto(32), &flags);
        assert!(result.is_ok());
    }

    #[test]
    fn protocol_30_allows_acls_and_xattrs() {
        let flags = ProtocolRestrictionFlags {
            preserve_acls: true,
            preserve_xattrs: true,
            ..Default::default()
        };
        assert!(refuse_unsupported_options(proto(30), &flags).is_ok());
    }

    #[test]
    fn protocol_29_rejects_acls() {
        let flags = ProtocolRestrictionFlags {
            preserve_acls: true,
            ..Default::default()
        };
        let err = refuse_unsupported_options(proto(29), &flags).unwrap_err();
        assert!(err.to_string().contains("--acls requires protocol 30"));
    }

    #[test]
    fn protocol_29_rejects_xattrs() {
        let flags = ProtocolRestrictionFlags {
            preserve_xattrs: true,
            ..Default::default()
        };
        let err = refuse_unsupported_options(proto(29), &flags).unwrap_err();
        assert!(err.to_string().contains("--xattrs requires protocol 30"));
    }

    #[test]
    fn protocol_29_allows_acls_for_local_server() {
        let flags = ProtocolRestrictionFlags {
            preserve_acls: true,
            local_server: true,
            ..Default::default()
        };
        assert!(refuse_unsupported_options(proto(29), &flags).is_ok());
    }

    #[test]
    fn protocol_28_rejects_fuzzy() {
        let flags = ProtocolRestrictionFlags {
            fuzzy_basis: true,
            ..Default::default()
        };
        let err = refuse_unsupported_options(proto(28), &flags).unwrap_err();
        assert!(err.to_string().contains("--fuzzy requires protocol 29"));
    }

    #[test]
    fn protocol_28_rejects_basis_dir_with_inplace() {
        let flags = ProtocolRestrictionFlags {
            basis_dir_count: 1,
            inplace: true,
            ..Default::default()
        };
        let err = refuse_unsupported_options(proto(28), &flags).unwrap_err();
        assert!(err.to_string().contains("--inplace requires protocol 29"));
    }

    #[test]
    fn protocol_28_rejects_multiple_basis_dirs() {
        let flags = ProtocolRestrictionFlags {
            basis_dir_count: 2,
            ..Default::default()
        };
        let err = refuse_unsupported_options(proto(28), &flags).unwrap_err();
        assert!(err.to_string().contains("more than one"));
    }

    #[test]
    fn protocol_28_rejects_prune_empty_dirs() {
        let flags = ProtocolRestrictionFlags {
            prune_empty_dirs: true,
            ..Default::default()
        };
        let err = refuse_unsupported_options(proto(28), &flags).unwrap_err();
        assert!(
            err.to_string()
                .contains("--prune-empty-dirs requires protocol 29")
        );
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
        assert!(refuse_unsupported_options(proto(29), &flags).is_ok());
    }

    #[test]
    fn protocol_28_single_basis_dir_without_inplace_ok() {
        let flags = ProtocolRestrictionFlags {
            basis_dir_count: 1,
            ..Default::default()
        };
        assert!(refuse_unsupported_options(proto(28), &flags).is_ok());
    }
}
