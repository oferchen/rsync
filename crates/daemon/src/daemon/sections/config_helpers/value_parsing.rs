/// Parses a comma/whitespace-separated list with deduplication.
///
/// Splits input by comma and whitespace, trims tokens, skips empty ones,
/// deduplicates based on a key function, and transforms tokens for storage.
fn parse_dedup_list<K, V, F, G>(
    value: &str,
    key_fn: F,
    value_fn: G,
    empty_error: &str,
) -> Result<Vec<V>, String>
where
    K: Eq + std::hash::Hash,
    F: Fn(&str) -> K,
    G: Fn(&str) -> V,
{
    let mut items = Vec::new();
    let mut seen = HashSet::new();

    for segment in value.split(',') {
        for token in segment.split_whitespace() {
            let trimmed = token.trim();
            if trimmed.is_empty() {
                continue;
            }

            let key = key_fn(trimmed);
            if seen.insert(key) {
                items.push(value_fn(trimmed));
            }
        }
    }

    if items.is_empty() {
        return Err(empty_error.to_owned());
    }

    Ok(items)
}

/// Parses a comma/whitespace-separated list of usernames with deduplication.
///
/// Usernames are case-preserved but deduplicated case-insensitively.
/// Group references using `@group` syntax are expanded to their member usernames.
///
/// # Access Level Suffixes
///
/// Entries may include an access level suffix:
/// - `:ro` - Read-only access (overrides module's read_only setting)
/// - `:rw` - Read-write access (overrides module's read_only setting)
/// - `:deny` - Deny access (authentication succeeds but access is blocked)
///
/// # Group Expansion
///
/// Entries starting with `@` are treated as system group names. The `@` prefix
/// is stripped and the group's members are looked up via `getgrnam_r`. All
/// members are added with the same access level as the @group entry.
/// Unknown groups are silently skipped (matching upstream rsync behavior).
///
/// # Examples
///
/// ```text
/// auth users = alice:rw, @staff:ro, bob:deny, charlie
/// ```
///
/// - alice has read-write access
/// - all members of @staff have read-only access
/// - bob is denied access
/// - charlie has default access (uses module settings)
pub(crate) fn parse_auth_user_list(value: &str) -> Result<Vec<AuthUser>, String> {
    let mut raw_entries = Vec::new();
    for segment in value.split(',') {
        for token in segment.split_whitespace() {
            let trimmed = token.trim();
            if !trimmed.is_empty() {
                raw_entries.push(trimmed.to_owned());
            }
        }
    }

    if raw_entries.is_empty() {
        return Err("must specify at least one username".to_owned());
    }

    let mut result = Vec::new();
    let mut seen = HashSet::new();

    for entry in raw_entries {
        let (name_part, access_level) = parse_access_suffix(&entry);

        if name_part.is_empty() {
            continue;
        }

        if let Some(group_name) = name_part.strip_prefix('@') {
            if group_name.is_empty() {
                continue;
            }
            // upstream: expand @group references to member usernames
            match lookup_group_members(group_name) {
                Ok(Some(members)) => {
                    for member in members {
                        let key = member.to_ascii_lowercase();
                        if seen.insert(key) {
                            result.push(AuthUser::with_access(member, access_level));
                        }
                    }
                }
                Ok(None) | Err(_) => {
                    // Group not found - silently skip (upstream behavior)
                }
            }
        } else {
            let key = name_part.to_ascii_lowercase();
            if seen.insert(key) {
                result.push(AuthUser::with_access(name_part.to_owned(), access_level));
            }
        }
    }

    if result.is_empty() {
        return Err("must specify at least one username".to_owned());
    }

    Ok(result)
}

/// Parses the access level suffix from a username entry.
///
/// Returns the username (without suffix) and the corresponding access level.
fn parse_access_suffix(entry: &str) -> (&str, UserAccessLevel) {
    if let Some(name) = entry.strip_suffix(":ro") {
        (name, UserAccessLevel::ReadOnly)
    } else if let Some(name) = entry.strip_suffix(":rw") {
        (name, UserAccessLevel::ReadWrite)
    } else if let Some(name) = entry.strip_suffix(":deny") {
        (name, UserAccessLevel::Deny)
    } else {
        (entry, UserAccessLevel::Default)
    }
}

/// Parses a comma/whitespace-separated list of refused options with deduplication.
///
/// Options are normalized to lowercase for both storage and deduplication.
pub(crate) fn parse_refuse_option_list(value: &str) -> Result<Vec<String>, String> {
    parse_dedup_list(
        value,
        |s| s.to_ascii_lowercase(),
        |s| s.to_ascii_lowercase(),
        "must specify at least one option",
    )
}

/// Parses a boolean value from a config directive.
///
/// Accepts common boolean representations: 1/0, true/false, yes/no, on/off.
/// Returns `None` for unrecognized values.
pub(crate) fn parse_boolean_directive(value: &str) -> Option<bool> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Parses a numeric identifier (uid/gid) from a config value.
pub(crate) fn parse_numeric_identifier(value: &str) -> Option<u32> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    trimmed.parse().ok()
}

/// Parses a timeout value in seconds.
///
/// Returns `Some(None)` for "0" (disabled), `Some(Some(n))` for valid timeouts,
/// or `None` for empty or invalid input.
pub(crate) fn parse_timeout_seconds(value: &str) -> Option<Option<NonZeroU64>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    let seconds: u64 = trimmed.parse().ok()?;
    if seconds == 0 {
        Some(None)
    } else {
        Some(NonZeroU64::new(seconds))
    }
}

/// Parses a max-connections directive value.
///
/// Returns `Some(None)` for "0" (unlimited), `Some(Some(n))` for a limit,
/// or `None` for empty or invalid input.
pub(crate) fn parse_max_connections_directive(value: &str) -> Option<Option<NonZeroU32>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed == "0" {
        return Some(None);
    }

    trimmed.parse::<NonZeroU32>().ok().map(Some)
}
