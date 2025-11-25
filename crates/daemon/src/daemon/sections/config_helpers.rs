pub(crate) fn parse_auth_user_list(value: &str) -> Result<Vec<String>, String> {
    let mut users = Vec::new();
    let mut seen = HashSet::new();

    for segment in value.split(',') {
        for token in segment.split_whitespace() {
            let trimmed = token.trim();
            if trimmed.is_empty() {
                continue;
            }

            if seen.insert(trimmed.to_ascii_lowercase()) {
                users.push(trimmed.to_string());
            }
        }
    }

    if users.is_empty() {
        return Err("must specify at least one username".to_string());
    }

    Ok(users)
}

pub(crate) fn parse_refuse_option_list(value: &str) -> Result<Vec<String>, String> {
    let mut options = Vec::new();
    let mut seen = HashSet::new();

    for segment in value.split(',') {
        for token in segment.split_whitespace() {
            let trimmed = token.trim();
            if trimmed.is_empty() {
                continue;
            }

            let canonical = trimmed.to_ascii_lowercase();
            if seen.insert(canonical.clone()) {
                options.push(canonical);
            }
        }
    }

    if options.is_empty() {
        return Err("must specify at least one option".to_string());
    }

    Ok(options)
}

fn validate_secrets_file(
    path: &Path,
    config_path: &Path,
    line: usize,
) -> Result<PathBuf, DaemonError> {
    let metadata = fs::metadata(path).map_err(|error| {
        config_parse_error(
            config_path,
            line,
            format!(
                "failed to access secrets file '{}': {}",
                path.display(),
                error
            ),
        )
    })?;

    if let Err(detail) = ensure_secrets_file(path, &metadata) {
        return Err(config_parse_error(config_path, line, detail));
    }

    Ok(path.to_path_buf())
}

fn validate_secrets_file_from_env(
    path: &Path,
    env: &'static str,
) -> Result<Option<PathBuf>, DaemonError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            if error.kind() == io::ErrorKind::NotFound {
                return Ok(None);
            }

            return Err(secrets_env_error(
                env,
                path,
                format!("could not be accessed: {error}"),
            ));
        }
    };

    if let Err(detail) = ensure_secrets_file(path, &metadata) {
        return Err(secrets_env_error(env, path, detail));
    }

    Ok(Some(path.to_path_buf()))
}

fn ensure_secrets_file(path: &Path, metadata: &fs::Metadata) -> Result<(), String> {
    if !metadata.is_file() {
        return Err(format!(
            "secrets file '{}' must be a regular file",
            path.display()
        ));
    }

    #[cfg(unix)]
    {
        let mode = metadata.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(format!(
                "secrets file '{}' must not be accessible to group or others (expected permissions 0600)",
                path.display()
            ));
        }
    }

    Ok(())
}

pub(crate) fn parse_boolean_directive(value: &str) -> Option<bool> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

pub(crate) fn parse_numeric_identifier(value: &str) -> Option<u32> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    trimmed.parse().ok()
}

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum HostPattern {
    Any,
    Ipv4 { network: Ipv4Addr, prefix: u8 },
    Ipv6 { network: Ipv6Addr, prefix: u8 },
    Hostname(HostnamePattern),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AddressFamily {
    Ipv4,
    Ipv6,
}

impl AddressFamily {
    fn from_ip(addr: IpAddr) -> Self {
        match addr {
            IpAddr::V4(_) => Self::Ipv4,
            IpAddr::V6(_) => Self::Ipv6,
        }
    }

    fn matches(self, addr: IpAddr) -> bool {
        matches!(
            (self, addr),
            (Self::Ipv4, IpAddr::V4(_)) | (Self::Ipv6, IpAddr::V6(_))
        )
    }
}

impl HostPattern {
    pub(crate) fn parse(token: &str) -> Result<Self, String> {
        let token = token.trim();
        if token.is_empty() {
            return Err("host pattern must be non-empty".to_string());
        }

        if token == "*" || token.eq_ignore_ascii_case("all") {
            return Ok(Self::Any);
        }

        let (address_str, prefix_text) = if let Some((addr, mask)) = token.split_once('/') {
            (addr, Some(mask))
        } else {
            (token, None)
        };

        if let Ok(ipv4) = address_str.parse::<Ipv4Addr>() {
            let prefix = prefix_text
                .map(|value| {
                    value
                        .parse::<u8>()
                        .map_err(|_| "invalid IPv4 prefix length".to_string())
                })
                .transpose()?;
            return Self::from_ipv4(ipv4, prefix.unwrap_or(32));
        }

        if let Ok(ipv6) = address_str.parse::<Ipv6Addr>() {
            let prefix = prefix_text
                .map(|value| {
                    value
                        .parse::<u8>()
                        .map_err(|_| "invalid IPv6 prefix length".to_string())
                })
                .transpose()?;
            return Self::from_ipv6(ipv6, prefix.unwrap_or(128));
        }

        if prefix_text.is_some() {
            return Err("invalid host pattern; expected IPv4/IPv6 address".to_string());
        }

        HostnamePattern::parse(address_str).map(Self::Hostname)
    }

    fn from_ipv4(addr: Ipv4Addr, prefix: u8) -> Result<Self, String> {
        if prefix > 32 {
            return Err("IPv4 prefix length must be between 0 and 32".to_string());
        }

        if prefix == 0 {
            return Ok(Self::Ipv4 {
                network: Ipv4Addr::UNSPECIFIED,
                prefix,
            });
        }

        let shift = 32 - u32::from(prefix);
        let mask = u32::MAX.checked_shl(shift).unwrap_or(0);
        let network = u32::from(addr) & mask;
        Ok(Self::Ipv4 {
            network: Ipv4Addr::from(network),
            prefix,
        })
    }

    fn from_ipv6(addr: Ipv6Addr, prefix: u8) -> Result<Self, String> {
        if prefix > 128 {
            return Err("IPv6 prefix length must be between 0 and 128".to_string());
        }

        if prefix == 0 {
            return Ok(Self::Ipv6 {
                network: Ipv6Addr::UNSPECIFIED,
                prefix,
            });
        }

        let shift = 128 - u32::from(prefix);
        let mask = u128::MAX.checked_shl(shift).unwrap_or(0);
        let network = u128::from(addr) & mask;
        Ok(Self::Ipv6 {
            network: Ipv6Addr::from(network),
            prefix,
        })
    }

    fn matches(&self, addr: IpAddr, hostname: Option<&str>) -> bool {
        match (self, addr) {
            (Self::Any, _) => true,
            (Self::Ipv4 { network, prefix }, IpAddr::V4(candidate)) => {
                if *prefix == 0 {
                    true
                } else {
                    let shift = 32 - u32::from(*prefix);
                    let mask = u32::MAX.checked_shl(shift).unwrap_or(0);
                    (u32::from(candidate) & mask) == u32::from(*network)
                }
            }
            (Self::Ipv6 { network, prefix }, IpAddr::V6(candidate)) => {
                if *prefix == 0 {
                    true
                } else {
                    let shift = 128 - u32::from(*prefix);
                    let mask = u128::MAX.checked_shl(shift).unwrap_or(0);
                    (u128::from(candidate) & mask) == u128::from(*network)
                }
            }
            (Self::Hostname(pattern), _) => {
                hostname.map(|name| pattern.matches(name)).unwrap_or(false)
            }
            _ => false,
        }
    }

    fn requires_hostname(&self) -> bool {
        matches!(self, Self::Hostname(_))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HostnamePattern {
    kind: HostnamePatternKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum HostnamePatternKind {
    Exact(String),
    Suffix(String),
    Wildcard(String),
}

impl HostnamePattern {
    fn parse(pattern: &str) -> Result<Self, String> {
        let trimmed = pattern.trim();
        if trimmed.is_empty() {
            return Err("host pattern must be non-empty".to_string());
        }

        let normalized = trimmed.trim_end_matches('.');
        let lower = normalized.to_ascii_lowercase();

        if lower.contains('*') || lower.contains('?') {
            return Ok(Self {
                kind: HostnamePatternKind::Wildcard(lower),
            });
        }

        if lower.starts_with('.') {
            let suffix = lower.trim_start_matches('.').to_string();
            return Ok(Self {
                kind: HostnamePatternKind::Suffix(suffix),
            });
        }

        Ok(Self {
            kind: HostnamePatternKind::Exact(lower),
        })
    }

    fn matches(&self, hostname: &str) -> bool {
        match &self.kind {
            HostnamePatternKind::Exact(expected) => hostname == expected,
            HostnamePatternKind::Suffix(suffix) => {
                if suffix.is_empty() {
                    return true;
                }

                if hostname == suffix {
                    return true;
                }

                if hostname.len() <= suffix.len() {
                    return false;
                }

                hostname.ends_with(suffix)
                    && hostname
                        .as_bytes()
                        .get(hostname.len() - suffix.len() - 1)
                        .is_some_and(|byte| *byte == b'.')
            }
            HostnamePatternKind::Wildcard(pattern) => wildcard_match(pattern, hostname),
        }
    }
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern_bytes = pattern.as_bytes();
    let text_bytes = text.as_bytes();

    let mut pat_index = 0usize;
    let mut text_index = 0usize;
    let mut star_index: Option<usize> = None;
    let mut match_index = 0usize;

    while text_index < text_bytes.len() {
        if pat_index < pattern_bytes.len()
            && (pattern_bytes[pat_index] == b'?'
                || pattern_bytes[pat_index] == text_bytes[text_index])
        {
            pat_index += 1;
            text_index += 1;
        } else if pat_index < pattern_bytes.len() && pattern_bytes[pat_index] == b'*' {
            // Record the position of the wildcard and optimistically advance past it.
            star_index = Some(pat_index);
            pat_index += 1;
            match_index = text_index;
        } else if let Some(star_pos) = star_index {
            // Retry the match by letting the last '*' consume one additional character.
            pat_index = star_pos + 1;
            match_index += 1;
            text_index = match_index;
        } else {
            return false;
        }
    }

    while pat_index < pattern_bytes.len() && pattern_bytes[pat_index] == b'*' {
        pat_index += 1;
    }

    pat_index == pattern_bytes.len()
}

fn parse_host_list(
    value: &str,
    config_path: &Path,
    line: usize,
    directive: &str,
) -> Result<Vec<HostPattern>, DaemonError> {
    let mut patterns = Vec::new();

    for token in value.split(|ch: char| ch.is_ascii_whitespace() || ch == ',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }

        let pattern = HostPattern::parse(token).map_err(|message| {
            config_parse_error(
                config_path,
                line,
                format!("{directive} directive contains invalid pattern '{token}': {message}"),
            )
        })?;
        patterns.push(pattern);
    }

    if patterns.is_empty() {
        return Err(config_parse_error(
            config_path,
            line,
            format!("{directive} directive must specify at least one pattern"),
        ));
    }

    Ok(patterns)
}
