//! Rsync daemon module listing functionality.
//!
//! This module provides support for querying rsync daemons to retrieve their
//! advertised module list, mirroring the behavior of `rsync rsync://host/` or
//! `rsync host::`. The implementation handles legacy `@RSYNCD:` protocol
//! negotiation, MOTD and capability parsing, and optional authentication.
//!
//! # Examples
//!
//! Requesting a module list from a daemon:
//!
//! ```ignore
//! use core::client::{ModuleListRequest, run_module_list};
//! use std::ffi::OsString;
//!
//! let operands = vec![OsString::from("rsync://example.com/")];
//! let request = ModuleListRequest::from_operands(&operands)?
//!     .expect("valid module list request");
//!
//! let list = run_module_list(request)?;
//! for entry in list.entries() {
//!     println!("{} - {}", entry.name(), entry.comment().unwrap_or(""));
//! }
//! ```

mod auth;
mod connect;
mod errors;
mod listing;
mod parsing;
mod request;
mod socket_options;
mod types;

pub use listing::{
    ModuleList, ModuleListEntry, run_module_list, run_module_list_with_options,
    run_module_list_with_password, run_module_list_with_password_and_options,
};
pub use request::{ModuleListOptions, ModuleListRequest};
pub use types::DaemonAddress;

#[allow(unused_imports)]
pub(super) use crate::auth::{DaemonAuthDigest, compute_daemon_auth_response};
#[allow(unused_imports)]
pub(super) use auth::{
    DaemonAuthContext, SensitiveBytes, load_daemon_password, send_daemon_auth_credentials,
};
#[allow(unused_imports)]
pub(super) use connect::{
    ConnectProgramConfig, ProxyConfig, ProxyCredentials, connect_direct, connect_via_proxy,
    establish_proxy_tunnel, parse_proxy_spec, resolve_connect_timeout, resolve_daemon_addresses,
};
#[allow(unused_imports)]
pub(super) use errors::map_daemon_handshake_error;
#[allow(unused_imports)]
pub(super) use parsing::parse_host_port;
