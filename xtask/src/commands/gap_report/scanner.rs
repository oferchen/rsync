#![deny(unsafe_code)]

//! Textual propagation scanner.
//!
//! The scanner answers a single, deterministic question about a receiver
//! config builder: does its source assign a given `ServerConfig` leaf field
//! from the client `config`? Upstream rsync gates receiver-applied options
//! behind `if (am_sender)` in `server_options()`, so those options are never
//! sent over the wire on a pull. The local client is the receiver and must
//! carry each such option onto its own `ServerConfig`. An option whose field is
//! never assigned from `config` is a latent propagation gap.
//!
//! The check is intentionally AST-lite: it scans for a whole-word assignment to
//! the leaf field (`... .<field> = <expr>;`) whose right-hand side references
//! `config`. This is robust against sub-string collisions (`delete` vs
//! `delete_excluded`), getter calls (`config.partial()` is not an assignment),
//! and comparison operators (`==`, `!=`, `<=`, `>=`).

/// Returns `true` when `source` assigns the `ServerConfig` leaf field `field`
/// from a `config` expression.
///
/// A match requires a whole-word occurrence of `field` immediately followed by
/// an `=` assignment (not a comparison) whose right-hand side, up to the
/// terminating `;`, references the identifier `config`.
pub fn field_assigned_from_config(source: &str, field: &str) -> bool {
    assignment_right_hand_sides(source, field).any(|rhs| references_identifier(rhs, "config"))
}

/// Yields the right-hand side (from just after `=` up to the next `;`) of each
/// whole-word assignment to `field` found in `source`.
fn assignment_right_hand_sides<'a>(
    source: &'a str,
    field: &'a str,
) -> impl Iterator<Item = &'a str> {
    let bytes = source.as_bytes();
    let mut cursor = 0usize;

    std::iter::from_fn(move || {
        while let Some(offset) = source[cursor..].find(field) {
            let start = cursor + offset;
            let end = start + field.len();
            cursor = end;

            if start > 0 && is_ident_byte(bytes[start - 1]) {
                continue;
            }
            if end < bytes.len() && is_ident_byte(bytes[end]) {
                continue;
            }

            let mut index = end;
            while index < bytes.len() && matches!(bytes[index], b' ' | b'\t') {
                index += 1;
            }
            if index >= bytes.len() || bytes[index] != b'=' {
                continue;
            }
            // Reject `==` (the only comparison whose first byte is `=`). Other
            // comparisons (`!=`, `<=`, `>=`) place a non-`=` byte before the
            // `=`, which the whitespace skip above never crosses.
            if bytes.get(index + 1) == Some(&b'=') {
                continue;
            }

            let rhs_start = index + 1;
            let rhs_end = source[rhs_start..]
                .find(';')
                .map_or(bytes.len(), |terminator| rhs_start + terminator);
            return Some(&source[rhs_start..rhs_end]);
        }
        None
    })
}

/// Returns `true` when `haystack` contains `identifier` as a whole word.
fn references_identifier(haystack: &str, identifier: &str) -> bool {
    let bytes = haystack.as_bytes();
    let mut cursor = 0usize;
    while let Some(offset) = haystack[cursor..].find(identifier) {
        let start = cursor + offset;
        let end = start + identifier.len();
        cursor = end;
        let left_ok = start == 0 || !is_ident_byte(bytes[start - 1]);
        let right_ok = end >= bytes.len() || !is_ident_byte(bytes[end]);
        if left_ok && right_ok {
            return true;
        }
    }
    false
}

/// Returns `true` when `byte` may appear inside a Rust identifier.
fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_statement_assignment_from_config() {
        let source = "    server_config.flags.list_only = config.list_only();";
        assert!(field_assigned_from_config(source, "list_only"));
    }

    #[test]
    fn detects_multiline_assignment_from_config() {
        let source =
            "server_config.backup_suffix = config\n    .backup_suffix()\n    .map(|s| s.into());";
        assert!(field_assigned_from_config(source, "backup_suffix"));
    }

    #[test]
    fn detects_wrapped_constructor_assignment() {
        let source =
            "server_config.flags.numeric_ids = NumericIds::from_client(config.numeric_ids());";
        assert!(field_assigned_from_config(source, "numeric_ids"));
    }

    #[test]
    fn missing_field_is_not_propagated() {
        let source = "server_config.flags.numeric_ids = config.numeric_ids();";
        assert!(!field_assigned_from_config(source, "list_only"));
    }

    #[test]
    fn getter_call_is_not_an_assignment() {
        // `config.partial()` reads the value but never assigns the field.
        let source = "let value = config.partial();";
        assert!(!field_assigned_from_config(source, "partial"));
    }

    #[test]
    fn substring_field_does_not_match_longer_identifier() {
        // `delete` must not match inside `delete_excluded` or `delete_mode`.
        let source = "server_config.deletion.delete_excluded = config.delete_mode().is_enabled();";
        assert!(!field_assigned_from_config(source, "delete"));
    }

    #[test]
    fn real_delete_assignment_matches() {
        let source = "server_config.flags.delete = config.delete_mode().is_enabled() || config.delete_excluded();";
        assert!(field_assigned_from_config(source, "delete"));
    }

    #[test]
    fn comparison_is_not_an_assignment() {
        let source = "if server_config.flags.list_only == config.list_only() { work(); }";
        assert!(!field_assigned_from_config(source, "list_only"));
    }

    #[test]
    fn assignment_from_literal_is_not_from_config() {
        // A hard-coded assignment does not carry the client's choice.
        let source = "server_config.flags.list_only = true;";
        assert!(!field_assigned_from_config(source, "list_only"));
    }

    #[test]
    fn partial_field_ignores_partial_dir_assignments() {
        let source = "server_config.write.partial_dir = config.partial_dir().map(Into::into);";
        assert!(!field_assigned_from_config(source, "partial"));
    }

    #[test]
    fn devices_field_ignores_preserve_devices_getter_shape() {
        let source = "server_config.flags.devices = config.preserve_devices();";
        assert!(field_assigned_from_config(source, "devices"));
    }

    #[test]
    fn rhs_does_not_leak_past_semicolon() {
        // The field is assigned from a literal; a later, unrelated statement
        // mentions config. The scanner must not join them across the `;`.
        let source = "server_config.flags.list_only = false; let x = config.foo();";
        assert!(!field_assigned_from_config(source, "list_only"));
    }
}
