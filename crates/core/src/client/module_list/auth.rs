#[cfg(test)]
use std::cell::RefCell;
use std::env;
use std::io::{BufReader, Write};

use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use rsync_checksums::strong::Md5;

use crate::client::{
    ClientError, DaemonAddress, daemon_authentication_required_error, socket_error,
};

pub(super) struct DaemonAuthContext {
    username: String,
    secret: SensitiveBytes,
}

impl DaemonAuthContext {
    pub(super) fn new(username: String, secret: Vec<u8>) -> Self {
        Self {
            username,
            secret: SensitiveBytes::new(secret),
        }
    }

    pub(super) fn username(&self) -> &str {
        &self.username
    }

    pub(super) fn secret(&self) -> &[u8] {
        self.secret.as_slice()
    }
}

#[cfg(test)]
impl DaemonAuthContext {
    pub(super) fn into_zeroized_secret(self) -> Vec<u8> {
        self.secret.into_zeroized_vec()
    }
}

pub(super) struct SensitiveBytes(Vec<u8>);

impl SensitiveBytes {
    pub(super) fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub(super) fn to_vec(&self) -> Vec<u8> {
        self.0.clone()
    }

    pub(super) fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(test)]
impl SensitiveBytes {
    pub(super) fn into_zeroized_vec(mut self) -> Vec<u8> {
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

pub(super) fn compute_daemon_auth_response(secret: &[u8], challenge: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(secret);
    hasher.update(challenge.as_bytes());
    let digest = hasher.finalize();
    STANDARD_NO_PAD.encode(digest)
}

#[cfg(test)]
thread_local! {
    static TEST_PASSWORD_OVERRIDE: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn set_test_daemon_password(password: Option<Vec<u8>>) {
    TEST_PASSWORD_OVERRIDE.with(|slot| *slot.borrow_mut() = password);
}

pub(super) fn load_daemon_password() -> Option<Vec<u8>> {
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

pub(super) fn send_daemon_auth_credentials<S>(
    reader: &mut BufReader<S>,
    context: &DaemonAuthContext,
    challenge: &str,
    addr: &DaemonAddress,
) -> Result<(), ClientError>
where
    S: Write,
{
    if context.username().is_empty() {
        return Err(daemon_authentication_required_error(
            "supply a username in the daemon URL (e.g. rsync://user@host/)",
        ));
    }

    let digest = compute_daemon_auth_response(context.secret(), challenge);
    let mut command = String::with_capacity(context.username().len() + digest.len() + 2);
    command.push_str(context.username());
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
