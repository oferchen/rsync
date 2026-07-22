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
/// Group references using `@group` syntax are stored verbatim and resolved to
/// membership at authentication time (see `authorize_auth_user`), matching
/// upstream `auth_server`.
///
/// # Access Level Suffixes
///
/// Entries may include an access level suffix:
/// - `:ro` - Read-only access (overrides module's read_only setting)
/// - `:rw` - Read-write access (overrides module's read_only setting)
/// - `:deny` - Deny access (authentication succeeds but access is blocked)
///
/// # Group Tokens
///
/// Entries starting with `@` name a system group. The token is kept verbatim
/// (`@group`) and, at authentication time, authorizes any connecting user who
/// is a member of that group. This mirrors upstream `auth_server`, which
/// resolves group membership per authenticating user rather than expanding the
/// group to a fixed member list at config load.
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

        // A bare `@` names no group and cannot authorize anyone; skip it.
        if name_part == "@" {
            continue;
        }

        // upstream: authenticate.c:276 keeps each token verbatim (including
        // `@group`) and matches it at auth time via wildmatch / group
        // membership. Do not expand `@group` to member usernames here.
        let key = name_part.to_ascii_lowercase();
        if seen.insert(key) {
            result.push(AuthUser::with_access(name_part.to_owned(), access_level));
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

/// Classification of a boolean directive value, mirroring the branches of
/// upstream `loadparm.c:set_boolean()`.
enum BooleanDirective {
    /// A concrete `true`/`false` value.
    Value(bool),
    /// The P_BOOL3 `unset`/`-1` tri-state (only recognized when unset is
    /// allowed).
    Unset,
    /// A value that `set_boolean()` rejects as badly formed.
    Malformed,
}

/// Classifies a boolean directive value the way upstream `set_boolean()` does.
///
/// upstream: loadparm.c:363-376. Accepts only `yes`/`true`/`1` and
/// `no`/`false`/`0` (case-insensitive); when `allow_unset` is set (P_BOOL3
/// params) also accepts `unset`/`-1` as a tri-state. Upstream does NOT accept
/// `on`/`off`.
fn classify_boolean_directive(value: &str, allow_unset: bool) -> BooleanDirective {
    match value.trim().to_ascii_lowercase().as_str() {
        "yes" | "true" | "1" => BooleanDirective::Value(true),
        "no" | "false" | "0" => BooleanDirective::Value(false),
        "unset" | "-1" if allow_unset => BooleanDirective::Unset,
        _ => BooleanDirective::Malformed,
    }
}

/// Parses a strict (non-tri-state) boolean directive value.
///
/// Mirrors the P_BOOL branch of upstream `loadparm.c:set_boolean()`: accepts
/// `yes`/`true`/`1` and `no`/`false`/`0` (case-insensitive) and nothing else.
/// Notably upstream does NOT accept `on`/`off`, so neither do we. Returns
/// `None` for any unrecognized value.
pub(crate) fn parse_boolean_directive(value: &str) -> Option<bool> {
    match classify_boolean_directive(value, false) {
        BooleanDirective::Value(flag) => Some(flag),
        BooleanDirective::Unset | BooleanDirective::Malformed => None,
    }
}

/// Applies upstream `set_boolean()`/`do_parameter()` semantics to a boolean
/// directive, reporting malformed values without aborting the config load.
///
/// `allow_unset` selects the P_BOOL3 tri-state (`unset`/`-1`). Returns
/// `Some(flag)` for a concrete value. Returns `None` when the value is the
/// BOOL3 `unset` tri-state (leave the setting unconfigured) or is malformed.
///
/// upstream: loadparm.c:418-423 - `do_parameter()` calls `set_boolean()` and
/// ignores its failure return, so a badly formed boolean only warns
/// (loadparm.c:372) and the directive's previous default is retained rather
/// than aborting the load.
pub(crate) fn apply_boolean_directive(
    value: &str,
    allow_unset: bool,
    directive: &str,
    path: &Path,
    line_number: usize,
) -> Option<bool> {
    match classify_boolean_directive(value, allow_unset) {
        BooleanDirective::Value(flag) => Some(flag),
        BooleanDirective::Unset => None,
        BooleanDirective::Malformed => {
            eprintln!(
                "warning: badly formed boolean in configuration file: '{value}' for '{directive}' in '{}' line {} [daemon={}]",
                path.display(),
                line_number,
                env!("CARGO_PKG_VERSION"),
            );
            None
        }
    }
}

/// Parses the leading integer of a config value the way C `atoi()` does.
///
/// upstream: loadparm.c:431-433 stores `atoi(parmvalue)` for P_INTEGER
/// directives. `atoi` skips leading whitespace, reads an optional sign followed
/// by the leading run of decimal digits, and stops at the first non-digit (so
/// `"5x"` yields `5`). It yields `0` when no digits are present. Overflow
/// saturates to the `i32` range rather than invoking C's undefined behaviour.
pub(crate) fn parse_atoi(value: &str) -> i32 {
    let bytes = value.trim_start().as_bytes();
    let mut index = 0;
    let negative = match bytes.first() {
        Some(b'+') => {
            index = 1;
            false
        }
        Some(b'-') => {
            index = 1;
            true
        }
        _ => false,
    };

    let mut magnitude: i64 = 0;
    while let Some(&byte) = bytes.get(index) {
        if !byte.is_ascii_digit() {
            break;
        }
        magnitude = magnitude
            .saturating_mul(10)
            .saturating_add(i64::from(byte - b'0'));
        index += 1;
    }

    let signed = if negative { -magnitude } else { magnitude };
    signed.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// Parses a numeric identifier (uid/gid) from a config value.
pub(crate) fn parse_numeric_identifier(value: &str) -> Option<u32> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    trimmed.parse().ok()
}

/// Resolves an rsyncd.conf `uid` directive to a numeric uid.
///
/// Accepts either a numeric id or a username. An all-digits value parses
/// directly as a numeric id; any other non-empty value is resolved through the
/// local NSS database (`getpwnam`). Returns `None` for an empty value or a name
/// that does not resolve, so the caller emits the same `invalid uid '<value>'`
/// config error it already produced for a bad numeric id.
///
/// upstream: uidlist.c:144 `user_to_uid()` called with `num_ok = True`
/// (clientserver.c:783) - a value made only of digits is `id_parse()`d, any
/// other value goes through `getpwnam()`.
pub(crate) fn parse_uid_setting(value: &str) -> Option<u32> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.bytes().all(|byte| byte.is_ascii_digit()) {
        return parse_numeric_identifier(trimmed);
    }

    metadata::id_lookup::lookup_user_by_name(trimmed.as_bytes())
        .ok()
        .flatten()
}

/// Parses a module `gid` directive into a [`GidSetting`].
///
/// Accepts a whitespace- or comma-separated list of numeric gids. A leading
/// `*` requests every group the target user belongs to; it must appear first,
/// and any following tokens are extra explicit gids.
///
/// upstream: clientserver.c:793-824 `rsync_module()` splits `lp_gid()` with
/// `conf_strtok`; `*` triggers `want_all_groups`, and each remaining token is
/// added via `add_a_group`. The whole list is later installed with
/// `setgroups`, clearing inherited supplementary groups.
pub(crate) fn parse_gid_setting(value: &str) -> Result<GidSetting, String> {
    let mut tokens = value
        .split([',', ' ', '\t'])
        .map(str::trim)
        .filter(|token| !token.is_empty());

    let Some(first) = tokens.next() else {
        return Err("directive is empty".to_owned());
    };

    let all_groups = first == "*";
    let mut extra = Vec::new();
    if !all_groups {
        extra.push(parse_gid_token(first)?);
    }

    for token in tokens {
        if token == "*" {
            return Err("'*' must be the first entry in the list".to_owned());
        }
        extra.push(parse_gid_token(token)?);
    }

    if all_groups {
        Ok(GidSetting::AllUserGroups { extra })
    } else {
        Ok(GidSetting::List(extra))
    }
}

/// Parses a single gid token, accepting either a numeric id or a group name.
///
/// An all-digits token parses directly as a numeric gid; any other token is
/// resolved through the local NSS database (`getgrnam`). An unresolvable token
/// is an error, matching the config value rejection upstream performs.
///
/// upstream: uidlist.c:170 `group_to_gid()` called with `num_ok = True`
/// (clientserver.c:807 `add_a_group()`) - a digits-only token is `id_parse()`d,
/// any other token goes through `getgrnam()`.
fn parse_gid_token(token: &str) -> Result<u32, String> {
    if token.bytes().all(|byte| byte.is_ascii_digit()) {
        return token
            .parse::<u32>()
            .map_err(|_| format!("'{token}' is not a valid gid"));
    }

    metadata::id_lookup::lookup_group_by_name(token.as_bytes())
        .ok()
        .flatten()
        .ok_or_else(|| format!("'{token}' is not a valid group name or gid"))
}

/// Parses a timeout directive value in seconds.
///
/// upstream: `timeout` is a P_INTEGER directive (daemon-parm.h:294), so the
/// value is read with `atoi()` leniency: a leading integer is parsed and
/// trailing non-digits are tolerated. A non-positive result (including an
/// empty or non-numeric value, which `atoi` maps to `0`) disables the timeout,
/// yielding `Some(None)`; a positive value yields `Some(Some(n))`. Never
/// returns `None`.
pub(crate) fn parse_timeout_seconds(value: &str) -> Option<Option<NonZeroU64>> {
    let seconds = parse_atoi(value).max(0) as u64;
    if seconds == 0 {
        Some(None)
    } else {
        Some(NonZeroU64::new(seconds))
    }
}

/// Parses a max-connections directive value.
///
/// upstream: `max connections` is a P_INTEGER directive (daemon-parm.h:292),
/// so the value is read with `atoi()` leniency: a leading integer is parsed and
/// trailing non-digits are tolerated. A non-positive result (including an empty
/// or non-numeric value, which `atoi` maps to `0`) means unlimited, yielding
/// `Some(None)`; a positive value yields `Some(Some(n))`. Never returns `None`.
pub(crate) fn parse_max_connections_directive(value: &str) -> Option<Option<NonZeroU32>> {
    let limit = parse_atoi(value).max(0) as u32;
    Some(NonZeroU32::new(limit))
}
