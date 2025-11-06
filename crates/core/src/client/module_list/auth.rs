#[cfg(test)]
use std::cell::RefCell;
use std::env;
use std::io::{BufReader, Write};

use crate::auth::{DaemonAuthDigest, compute_daemon_auth_response};

use super::super::{ClientError, socket_error};
use super::types::DaemonAddress;

pub(crate) struct DaemonAuthContext {
    username: String,
    secret: SensitiveBytes,
    digest: DaemonAuthDigest,
}

impl DaemonAuthContext {
    pub(crate) fn new(username: String, secret: Vec<u8>, digest: DaemonAuthDigest) -> Self {
        Self {
            username,
            secret: SensitiveBytes::new(secret),
            digest,
        }
    }

    pub(crate) fn secret(&self) -> &[u8] {
        self.secret.as_slice()
    }

    pub(crate) fn digest(&self) -> DaemonAuthDigest {
        self.digest
    }

    #[cfg(test)]
    pub(crate) fn into_zeroized_secret(self) -> Vec<u8> {
        self.secret.into_zeroized_vec()
    }
}

pub(crate) struct SensitiveBytes(Vec<u8>);

impl SensitiveBytes {
    pub(crate) fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub(crate) fn to_vec(&self) -> Vec<u8> {
        self.0.clone()
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        &self.0
    }

    #[cfg(test)]
    pub(crate) fn into_zeroized_vec(mut self) -> Vec<u8> {
        for byte in &mut self.0 {
            *byte = 0;
        }
        std::mem::take(&mut self.0)
    }
}

impl Drop for SensitiveBytes {
    fn drop(&mut self) {
        for byte in &mut self.0 {
            *byte = 0;
        }
    }
}

pub(crate) fn send_daemon_auth_credentials<S>(
    reader: &mut BufReader<S>,
    context: &DaemonAuthContext,
    challenge: &str,
    addr: &DaemonAddress,
) -> Result<(), ClientError>
where
    S: Write,
{
    let digest = compute_daemon_auth_response(context.secret(), challenge, context.digest());
    let mut command = String::with_capacity(context.username.len() + digest.len() + 2);
    command.push_str(&context.username);
    command.push(' ');
    command.push_str(&digest);
    command.push('\n');

    reader
        .get_mut()
        .write_all(command.as_bytes())
        .map_err(|error| socket_error("write to", addr.socket_addr_display(), error))?;
    reader
        .get_mut()
        .flush()
        .map_err(|error| socket_error("flush", addr.socket_addr_display(), error))?;

    Ok(())
}

#[cfg(test)]
thread_local! {
    static TEST_PASSWORD_OVERRIDE: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_test_daemon_password(password: Option<Vec<u8>>) {
    TEST_PASSWORD_OVERRIDE.with(|slot| *slot.borrow_mut() = password);
}

pub(crate) fn load_daemon_password() -> Option<Vec<u8>> {
    #[cfg(test)]
    if let Some(password) = TEST_PASSWORD_OVERRIDE.with(|slot| slot.borrow().clone()) {
        return Some(password);
    }

    env::var_os("RSYNC_PASSWORD").map(|value| {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;

            value.into_vec()
        }

        #[cfg(not(unix))]
        {
            value.to_string_lossy().into_owned().into_bytes()
        }
    })
}

pub(crate) fn normalize_motd_payload(payload: &str) -> String {
    if !is_motd_payload(payload) {
        return payload.to_string();
    }

    let remainder = &payload[4..];
    let remainder = remainder.trim_start_matches([' ', '\t', ':']);
    remainder.trim_start().to_string()
}

pub(crate) fn is_motd_payload(payload: &str) -> bool {
    let bytes = payload.as_bytes();
    if bytes.len() < 4 {
        return false;
    }

    if !bytes[..4].eq_ignore_ascii_case(b"motd") {
        return false;
    }

    if bytes.len() == 4 {
        return true;
    }

    matches!(bytes[4], b' ' | b'\t' | b'\r' | b'\n' | b':')
}
