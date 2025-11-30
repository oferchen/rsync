use std::ffi::{OsStr, OsString};
use std::io::{self, ErrorKind};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};

use core::{
    client::BindAddress,
    message::{Message, Role, strings},
    rsync_error,
};

use super::super::defaults::SUPPORTED_OPTIONS_LIST;

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
