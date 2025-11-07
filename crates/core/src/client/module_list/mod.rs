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
#[cfg(test)]
pub(super) use auth::set_test_daemon_password;
#[allow(unused_imports)]
pub(super) use auth::{DaemonAuthContext, SensitiveBytes, send_daemon_auth_credentials};
#[allow(unused_imports)]
pub(super) use connect::{
    ConnectProgramConfig, ProxyConfig, ProxyCredentials, connect_direct, connect_via_proxy,
    establish_proxy_tunnel, parse_proxy_spec, resolve_connect_timeout, resolve_daemon_addresses,
};
#[allow(unused_imports)]
pub(super) use errors::map_daemon_handshake_error;
#[cfg(test)]
pub(crate) use socket_options::apply_socket_options;
