use std::ffi::{OsStr, OsString};
use std::io::{self, ErrorKind};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};

use core::{
    client::BindAddress,
    message::{Message, Role, strings},
    rsync_error,
};

use super::super::defaults::SUPPORTED_OPTIONS_LIST;

#[derive(Debug)]
pub(crate) struct UnsupportedOption {
    option: OsString,
}

impl UnsupportedOption {
    pub(crate) fn new(option: OsString) -> Self {
        Self { option }
    }

    pub(crate) fn to_message(&self) -> Message {
        let option = self.option.to_string_lossy();
        let text = format!(
            "unknown option '{option}': this build currently supports only {SUPPORTED_OPTIONS_LIST}"
        );
        strings::exit_code_message_with_detail(1, text.clone())
            .unwrap_or_else(|| rsync_error!(1, text))
            .with_role(Role::Client)
    }

    /// Deprecated: Kept for reference, will be removed once native SSH is fully validated
    #[allow(dead_code)]
    pub(crate) fn fallback_text(&self) -> String {
        let option = self.option.to_string_lossy();
        format!(
            "unknown option '{option}': this build currently supports only {SUPPORTED_OPTIONS_LIST}"
        )
    }
}

fn is_option(argument: &OsStr) -> bool {
    let text = argument.to_string_lossy();
    let mut chars = text.chars();
    matches!(chars.next(), Some('-')) && chars.next().is_some()
}

pub(crate) fn extract_operands(
    arguments: Vec<OsString>,
) -> Result<Vec<OsString>, UnsupportedOption> {
    let mut operands = Vec::new();
    let mut accept_everything = false;

    for argument in arguments {
        if !accept_everything {
            if argument == "--" {
                accept_everything = true;
                continue;
            }

            if is_option(argument.as_os_str()) {
                return Err(UnsupportedOption::new(argument));
            }
        }

        operands.push(argument);
    }

    Ok(operands)
}

pub(crate) fn parse_bind_address_argument(value: &OsStr) -> Result<BindAddress, Message> {
    if value.is_empty() {
        return Err(rsync_error!(1, "--address requires a non-empty value").with_role(Role::Client));
    }

    let text = value.to_string_lossy();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(rsync_error!(1, "--address requires a non-empty value").with_role(Role::Client));
    }

    match resolve_bind_address(trimmed) {
        Ok(socket) => Ok(BindAddress::new(value.to_os_string(), socket)),
        Err(error) => {
            let formatted = format!("failed to resolve --address value '{trimmed}': {error}");
            Err(rsync_error!(1, formatted).with_role(Role::Client))
        }
    }
}

fn resolve_bind_address(text: &str) -> io::Result<SocketAddr> {
    if let Ok(ip) = text.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, 0));
    }

    let candidate = if text.starts_with('[') {
        format!("{text}:0")
    } else if text.contains(':') {
        format!("[{text}]:0")
    } else {
        format!("{text}:0")
    };

    let mut resolved = candidate.to_socket_addrs()?;
    resolved
        .next()
        .map(|addr| SocketAddr::new(addr.ip(), 0))
        .ok_or_else(|| {
            io::Error::new(
                ErrorKind::AddrNotAvailable,
                "address resolution returned no results",
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== is_option tests ====================

    #[test]
    fn is_option_single_dash_with_letter() {
        assert!(is_option(OsStr::new("-a")));
        assert!(is_option(OsStr::new("-v")));
        assert!(is_option(OsStr::new("-z")));
    }

    #[test]
    fn is_option_double_dash_long_option() {
        assert!(is_option(OsStr::new("--archive")));
        assert!(is_option(OsStr::new("--verbose")));
        assert!(is_option(OsStr::new("--delete")));
    }

    #[test]
    fn is_option_single_dash_alone() {
        // Single dash alone is NOT an option (it represents stdin/stdout)
        assert!(!is_option(OsStr::new("-")));
    }

    #[test]
    fn is_option_double_dash_alone() {
        // Double dash alone is NOT an option (it's the option terminator)
        assert!(is_option(OsStr::new("--")));
    }

    #[test]
    fn is_option_combined_short_options() {
        assert!(is_option(OsStr::new("-avz")));
        assert!(is_option(OsStr::new("-rltDz")));
    }

    #[test]
    fn is_option_regular_path() {
        assert!(!is_option(OsStr::new("file.txt")));
        assert!(!is_option(OsStr::new("path/to/file")));
        assert!(!is_option(OsStr::new("/absolute/path")));
    }

    #[test]
    fn is_option_empty_string() {
        assert!(!is_option(OsStr::new("")));
    }

    #[test]
    fn is_option_option_with_value() {
        assert!(is_option(OsStr::new("--port=8873")));
        assert!(is_option(OsStr::new("--config=/etc/rsyncd.conf")));
    }

    // ==================== extract_operands tests ====================

    #[test]
    fn extract_operands_no_options() {
        let args = vec![
            OsString::from("source/"),
            OsString::from("dest/"),
        ];
        let result = extract_operands(args);
        assert!(result.is_ok());
        let operands = result.unwrap();
        assert_eq!(operands.len(), 2);
        assert_eq!(operands[0], "source/");
        assert_eq!(operands[1], "dest/");
    }

    #[test]
    fn extract_operands_with_unknown_option() {
        let args = vec![
            OsString::from("-x"),
            OsString::from("source/"),
        ];
        let result = extract_operands(args);
        assert!(result.is_err());
    }

    #[test]
    fn extract_operands_after_double_dash() {
        let args = vec![
            OsString::from("--"),
            OsString::from("-x"),
            OsString::from("source/"),
        ];
        let result = extract_operands(args);
        assert!(result.is_ok());
        let operands = result.unwrap();
        // -x is accepted as operand after --
        assert_eq!(operands.len(), 2);
        assert_eq!(operands[0], "-x");
        assert_eq!(operands[1], "source/");
    }

    #[test]
    fn extract_operands_double_dash_stripped() {
        let args = vec![
            OsString::from("--"),
            OsString::from("file"),
        ];
        let result = extract_operands(args);
        assert!(result.is_ok());
        let operands = result.unwrap();
        // The -- itself should not be in operands
        assert_eq!(operands.len(), 1);
        assert_eq!(operands[0], "file");
    }

    #[test]
    fn extract_operands_empty_input() {
        let args: Vec<OsString> = vec![];
        let result = extract_operands(args);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn extract_operands_single_dash_allowed() {
        // Single dash (stdin/stdout) is not an option
        let args = vec![
            OsString::from("-"),
            OsString::from("dest/"),
        ];
        let result = extract_operands(args);
        assert!(result.is_ok());
        let operands = result.unwrap();
        assert_eq!(operands.len(), 2);
        assert_eq!(operands[0], "-");
    }

    #[test]
    fn extract_operands_long_option_rejected() {
        let args = vec![
            OsString::from("--unknown"),
            OsString::from("dest/"),
        ];
        let result = extract_operands(args);
        assert!(result.is_err());
    }

    #[test]
    fn extract_operands_option_with_value_rejected() {
        let args = vec![
            OsString::from("--port=8873"),
            OsString::from("dest/"),
        ];
        let result = extract_operands(args);
        assert!(result.is_err());
    }

    // ==================== UnsupportedOption tests ====================

    #[test]
    fn unsupported_option_to_message_contains_option() {
        let unsupported = UnsupportedOption::new(OsString::from("--unknown-opt"));
        let message = unsupported.to_message();
        let text = format!("{}", message);
        assert!(text.contains("--unknown-opt"));
    }

    #[test]
    fn unsupported_option_fallback_text_contains_option() {
        let unsupported = UnsupportedOption::new(OsString::from("-xyz"));
        let text = unsupported.fallback_text();
        assert!(text.contains("-xyz"));
    }

    // ==================== parse_bind_address_argument tests ====================

    #[test]
    fn parse_bind_address_empty_value() {
        let result = parse_bind_address_argument(OsStr::new(""));
        assert!(result.is_err());
    }

    #[test]
    fn parse_bind_address_whitespace_only() {
        let result = parse_bind_address_argument(OsStr::new("   "));
        assert!(result.is_err());
    }

    #[test]
    fn parse_bind_address_ipv4() {
        let result = parse_bind_address_argument(OsStr::new("127.0.0.1"));
        assert!(result.is_ok());
        let addr = result.unwrap();
        assert_eq!(addr.raw().to_string_lossy(), "127.0.0.1");
    }

    #[test]
    fn parse_bind_address_ipv6() {
        let result = parse_bind_address_argument(OsStr::new("::1"));
        assert!(result.is_ok());
    }

    #[test]
    fn parse_bind_address_ipv4_any() {
        let result = parse_bind_address_argument(OsStr::new("0.0.0.0"));
        assert!(result.is_ok());
    }
}
