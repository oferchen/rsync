//! Session identity for the daemon's `uid` / `gid` module settings on Windows.
//!
//! upstream: clientserver.c:1096-1135 `rsync_module()` drops the forked
//! session child to the module's identity with `setgid()`, `setgroups()` and
//! `setuid()`, refusing the session with `@ERROR: <op> failed` when a call
//! fails. Windows has no in-place identity switch for a running process, so
//! the equivalent is a token for the configured account, obtained without a
//! password through a service-for-user (S4U) logon, under which the session
//! process is created. The S4U logon needs `SeTcbPrivilege`, which the daemon
//! holds when it runs as the LocalSystem service.
//!
//! The pieces that decide refusal text and buffer layout are plain functions,
//! testable on every platform; the logon itself is Windows-only.

use std::fmt;
use std::io;

/// Upstream identity call that a Windows logon step stands in for.
///
/// The session refusal names it exactly as upstream does, one line per
/// call, so an operator reads the same `@ERROR` on either platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityOp {
    /// upstream: clientserver.c:1098-1101 `setgid(gid_array[0])`.
    Setgid,
    /// upstream: clientserver.c:1105-1108 `setgroups()`.
    Setgroups,
    /// upstream: clientserver.c:1123-1130 `setuid()` / `seteuid()`.
    Setuid,
}

impl IdentityOp {
    /// The call's name as upstream prints it in `@ERROR: <op> failed`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Setgid => "setgid",
            Self::Setgroups => "setgroups",
            Self::Setuid => "setuid",
        }
    }
}

/// Why the session identity could not be established.
#[derive(Debug)]
pub enum SessionIdentityError {
    /// The `uid` setting names no user account.
    ///
    /// upstream: clientserver.c:835-838 prints `Invalid uid %s` to the log and
    /// `@ERROR: invalid uid %s` to the client.
    InvalidUid(String),
    /// A `gid` entry names no group.
    ///
    /// upstream: clientserver.c:692-701 `add_a_group()` prints `Invalid gid %s`
    /// and `@ERROR: invalid gid %s`.
    InvalidGid(String),
    /// The step standing in for `op` failed.
    Failed {
        /// The upstream call the failed step corresponds to.
        op: IdentityOp,
        /// The underlying Win32 or LSA error.
        source: io::Error,
    },
}

impl fmt::Display for SessionIdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUid(name) => write!(f, "invalid uid {name}"),
            Self::InvalidGid(name) => write!(f, "invalid gid {name}"),
            Self::Failed { op, source } => write!(f, "{} failed: {source}", op.as_str()),
        }
    }
}

impl std::error::Error for SessionIdentityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Failed { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// The `gid` setting split the way upstream reads it.
///
/// upstream: clientserver.c:844-871 - a leading `*` asks for every group the
/// account belongs to; further names are added after it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct GroupRequest {
    /// `true` when the first entry was `*`.
    pub account_groups: bool,
    /// Explicitly named groups, in configuration order.
    pub names: Vec<String>,
}

impl GroupRequest {
    /// Parses the module's `gid` entries, already split on whitespace and
    /// commas as `conf_strtok()` does.
    ///
    /// Only a leading `*` is special; a later one is an ordinary (and
    /// unresolvable) group name, as in upstream.
    #[must_use]
    pub fn parse<'a>(entries: impl IntoIterator<Item = &'a str>) -> Self {
        let mut request = Self::default();
        for (index, entry) in entries.into_iter().enumerate() {
            if index == 0 && entry == "*" {
                request.account_groups = true;
            } else {
                request.names.push(entry.to_owned());
            }
        }
        request
    }

    /// Whether the setting asks for any group at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.account_groups && self.names.is_empty()
    }

    /// The group that becomes the token's primary group, if any.
    ///
    /// upstream: clientserver.c:1098 `setgid(gid_array[0])`. With `*` the
    /// first entry is the account's own primary group, so it stays as is.
    #[must_use]
    pub fn primary(&self) -> Option<&str> {
        if self.account_groups {
            None
        } else {
            self.names.first().map(String::as_str)
        }
    }
}

/// The upstream call a logon refused before any token exists reports as.
///
/// A missing `SeTcbPrivilege` refuses every step at once. Upstream applies
/// `setgid()` before `setuid()` (clientserver.c:1096 then :1122), so a daemon
/// unable to switch identity fails at `setgid` when groups were requested and
/// at `setuid` otherwise.
#[must_use]
pub const fn first_refused_op(groups_requested: bool) -> IdentityOp {
    if groups_requested {
        IdentityOp::Setgid
    } else {
        IdentityOp::Setuid
    }
}

/// Lays out an S4U logon buffer: a `header_len`-byte header followed by each
/// string's UTF-16 code units, every string starting on a 2-byte boundary.
///
/// Returns the zeroed buffer with the strings copied in and each string's
/// byte offset. The header is left for the caller to fill, because it holds
/// pointers into this same buffer.
#[must_use]
pub fn pack_s4u_strings(header_len: usize, strings: &[&[u16]]) -> (Vec<u8>, Vec<usize>) {
    let mut len = header_len.next_multiple_of(2);
    let mut offsets = Vec::with_capacity(strings.len());
    for string in strings {
        offsets.push(len);
        len += string.len() * 2;
    }
    let mut buffer = vec![0_u8; len];
    for (string, &offset) in strings.iter().zip(&offsets) {
        for (index, unit) in string.iter().enumerate() {
            let at = offset + index * 2;
            buffer[at..at + 2].copy_from_slice(&unit.to_ne_bytes());
        }
    }
    (buffer, offsets)
}

#[cfg(windows)]
pub use imp::{SessionToken, logon_session_identity};

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::io;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
    use std::ptr;

    use windows::Win32::Foundation::{HANDLE, LUID, NTSTATUS};
    use windows::Win32::Security::Authentication::Identity::{
        KERB_S4U_LOGON, KerbS4ULogon, LSA_STRING, LSA_UNICODE_STRING, LsaDeregisterLogonProcess,
        LsaFreeReturnBuffer, LsaLogonUser, LsaLookupAuthenticationPackage, LsaNtStatusToWinError,
        LsaRegisterLogonProcess, MSV1_0_S4U_LOGON, MsV1_0S4ULogon, SECURITY_LOGON_TYPE,
    };
    use windows::Win32::Security::{
        AllocateLocallyUniqueId, LookupAccountNameW, PSID, QUOTA_LIMITS, SID_AND_ATTRIBUTES,
        SID_NAME_USE, SetTokenInformation, SidTypeAlias, SidTypeGroup, SidTypeUser,
        SidTypeWellKnownGroup, TOKEN_GROUPS, TOKEN_PRIMARY_GROUP, TOKEN_SOURCE, TokenPrimaryGroup,
    };
    use windows::Win32::System::SystemInformation::{ComputerNameNetBIOS, GetComputerNameExW};
    use windows::core::{PCWSTR, PSTR, PWSTR};

    use super::{
        GroupRequest, IdentityOp, SessionIdentityError, first_refused_op, pack_s4u_strings,
    };

    // winnt.h group attribute bits for a group added to the logon token.
    const SE_GROUP_MANDATORY: u32 = 0x1;
    const SE_GROUP_ENABLED_BY_DEFAULT: u32 = 0x2;
    const SE_GROUP_ENABLED: u32 = 0x4;

    /// Name the daemon registers with the LSA and stamps on its tokens.
    const LOGON_PROCESS_NAME: &[u8] = b"oc-rsync";

    /// A primary token for the module's configured account.
    ///
    /// The session process is created under this token; dropping it closes
    /// the handle.
    #[derive(Debug)]
    pub struct SessionToken {
        handle: OwnedHandle,
    }

    impl SessionToken {
        /// The raw token handle, for process creation.
        #[must_use]
        pub fn as_raw_handle(&self) -> RawHandle {
            self.handle.as_raw_handle()
        }
    }

    /// One resolved account: its SID bytes and the authority that issued it.
    struct Account {
        sid: Vec<u8>,
        domain: String,
        kind: SID_NAME_USE,
    }

    /// Logs on the module's `uid` account through S4U and applies `groups`.
    ///
    /// The resulting token carries the account's own groups plus every named
    /// group, and the first named group as its primary group (unless the
    /// setting starts with `*`).
    ///
    /// # Errors
    ///
    /// [`SessionIdentityError::InvalidUid`] / [`SessionIdentityError::InvalidGid`]
    /// when a name resolves to no account of the right kind;
    /// [`SessionIdentityError::Failed`] naming the upstream call the failing
    /// step stands in for otherwise.
    pub fn logon_session_identity(
        user: &str,
        groups: &GroupRequest,
    ) -> Result<SessionToken, SessionIdentityError> {
        let account = lookup_account(user)
            .filter(|account| account.kind == SidTypeUser)
            .ok_or_else(|| SessionIdentityError::InvalidUid(user.to_owned()))?;
        let mut group_sids = Vec::with_capacity(groups.names.len());
        for name in &groups.names {
            let group = lookup_account(name)
                .filter(|group| is_group_kind(group.kind))
                .ok_or_else(|| SessionIdentityError::InvalidGid(name.clone()))?;
            group_sids.push(group.sid);
        }

        let privileged_op = first_refused_op(!groups.is_empty());
        let lsa = LsaSession::register().map_err(|source| SessionIdentityError::Failed {
            op: privileged_op,
            source,
        })?;
        let token = lsa
            .s4u_logon(user, &account, &group_sids)
            .map_err(|source| SessionIdentityError::Failed {
                op: if group_sids.is_empty() {
                    IdentityOp::Setuid
                } else {
                    IdentityOp::Setgroups
                },
                source,
            })?;
        if groups.primary().is_some() {
            set_primary_group(&token, &group_sids[0]).map_err(|source| {
                SessionIdentityError::Failed {
                    op: IdentityOp::Setgid,
                    source,
                }
            })?;
        }
        Ok(token)
    }

    fn is_group_kind(kind: SID_NAME_USE) -> bool {
        kind == SidTypeGroup || kind == SidTypeAlias || kind == SidTypeWellKnownGroup
    }

    fn to_wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    #[allow(unsafe_code)]
    fn lookup_account(name: &str) -> Option<Account> {
        let name_wide = to_wide(name);
        let mut sid_size: u32 = 0;
        let mut domain_size: u32 = 0;
        let mut kind = SID_NAME_USE::default();
        // SAFETY: null buffers with zero sizes is the documented sizing call;
        // the API only writes the two sizes and `kind`.
        unsafe {
            let _ = LookupAccountNameW(
                PCWSTR::null(),
                PCWSTR(name_wide.as_ptr()),
                None,
                &mut sid_size,
                None,
                &mut domain_size,
                &mut kind,
            );
        }
        if sid_size == 0 {
            return None;
        }
        let mut sid = vec![0_u8; sid_size as usize];
        let mut domain = vec![0_u16; domain_size as usize];
        // SAFETY: both buffers are sized by the call above and outlive this
        // call; the API writes at most `sid_size` / `domain_size` elements.
        let found = unsafe {
            LookupAccountNameW(
                PCWSTR::null(),
                PCWSTR(name_wide.as_ptr()),
                Some(PSID(sid.as_mut_ptr().cast())),
                &mut sid_size,
                Some(PWSTR(domain.as_mut_ptr())),
                &mut domain_size,
                &mut kind,
            )
        };
        found.ok()?;
        domain.truncate(domain_size as usize);
        Some(Account {
            sid,
            domain: String::from_utf16_lossy(&domain),
            kind,
        })
    }

    #[allow(unsafe_code)]
    fn computer_name() -> io::Result<String> {
        let mut size: u32 = 0;
        // SAFETY: a null buffer with zero size is the documented sizing call.
        let _ = unsafe { GetComputerNameExW(ComputerNameNetBIOS, None, &mut size) };
        let mut buffer = vec![0_u16; size as usize];
        // SAFETY: `buffer` holds `size` code units, as the sizing call asked.
        unsafe {
            GetComputerNameExW(
                ComputerNameNetBIOS,
                Some(PWSTR(buffer.as_mut_ptr())),
                &mut size,
            )
        }
        .map_err(io::Error::other)?;
        buffer.truncate(size as usize);
        Ok(String::from_utf16_lossy(&buffer))
    }

    #[allow(unsafe_code)]
    fn nt_error(status: NTSTATUS) -> io::Error {
        // SAFETY: a pure status-code translation with no pointer arguments.
        let code = unsafe { LsaNtStatusToWinError(status) };
        io::Error::from_raw_os_error(code as i32)
    }

    fn lsa_string(bytes: &[u8]) -> LSA_STRING {
        LSA_STRING {
            Length: bytes.len() as u16,
            MaximumLength: bytes.len() as u16,
            Buffer: PSTR(bytes.as_ptr().cast_mut()),
        }
    }

    /// A trusted LSA connection, deregistered on drop.
    struct LsaSession {
        handle: HANDLE,
    }

    impl LsaSession {
        /// Registers the daemon as a logon process.
        ///
        /// This is the step that needs `SeTcbPrivilege`; without it the LSA
        /// refuses and no logon can follow.
        #[allow(unsafe_code)]
        fn register() -> io::Result<Self> {
            let name = lsa_string(LOGON_PROCESS_NAME);
            let mut handle = HANDLE::default();
            let mut mode: u32 = 0;
            // SAFETY: `name` borrows a static byte string; `handle` and
            // `mode` are out-parameters owned by this frame.
            let status = unsafe { LsaRegisterLogonProcess(&name, &mut handle, &mut mode) };
            if status.is_err() {
                return Err(nt_error(status));
            }
            Ok(Self { handle })
        }

        #[allow(unsafe_code)]
        fn package(&self, name: &[u8]) -> io::Result<u32> {
            let name = lsa_string(name);
            let mut package: u32 = 0;
            // SAFETY: `self.handle` is a registered LSA handle; `name` and
            // `package` live for the call.
            let status =
                unsafe { LsaLookupAuthenticationPackage(self.handle, &name, &mut package) };
            if status.is_err() {
                return Err(nt_error(status));
            }
            Ok(package)
        }

        /// Performs the S4U logon for `account`, adding `extra_groups`.
        ///
        /// Local accounts go through MSV1_0, domain accounts through Kerberos
        /// with a `user@domain` principal name.
        #[allow(unsafe_code)]
        fn s4u_logon(
            &self,
            user: &str,
            account: &Account,
            extra_groups: &[Vec<u8>],
        ) -> io::Result<super::SessionToken> {
            let bare_user = user.rsplit_once('\\').map_or(user, |(_, name)| name);
            let local = account.domain.eq_ignore_ascii_case(&computer_name()?);
            let (package, mut buffer) = if local {
                let upn: Vec<u16> = bare_user.encode_utf16().collect();
                let domain: Vec<u16> = account.domain.encode_utf16().collect();
                let (mut buffer, offsets) =
                    pack_s4u_strings(size_of::<MSV1_0_S4U_LOGON>(), &[&upn, &domain]);
                let base = buffer.as_mut_ptr();
                let header = MSV1_0_S4U_LOGON {
                    MessageType: MsV1_0S4ULogon,
                    Flags: 0,
                    // SAFETY: each offset lies inside `buffer`, which the
                    // header points into and which outlives the logon call.
                    UserPrincipalName: unicode(unsafe { base.add(offsets[0]) }, upn.len()),
                    // SAFETY: as above.
                    DomainName: unicode(unsafe { base.add(offsets[1]) }, domain.len()),
                };
                write_header(&mut buffer, header);
                (self.package(MSV1_0_PACKAGE_NAME_BYTES)?, buffer)
            } else {
                let upn: Vec<u16> = format!("{bare_user}@{}", account.domain)
                    .encode_utf16()
                    .collect();
                let (mut buffer, offsets) = pack_s4u_strings(size_of::<KERB_S4U_LOGON>(), &[&upn]);
                let base = buffer.as_mut_ptr();
                let header = KERB_S4U_LOGON {
                    MessageType: KerbS4ULogon,
                    Flags: 0,
                    // SAFETY: the offset lies inside `buffer`, which outlives
                    // the logon call.
                    ClientUpn: unicode(unsafe { base.add(offsets[0]) }, upn.len()),
                    ClientRealm: LSA_UNICODE_STRING::default(),
                };
                write_header(&mut buffer, header);
                (self.package(KERBEROS_NAME_BYTES)?, buffer)
            };

            let mut sids = extra_groups.to_vec();
            let groups_buffer = token_groups(&mut sids);
            let origin = lsa_string(LOGON_PROCESS_NAME);
            let mut source = TOKEN_SOURCE::default();
            for (slot, byte) in source.SourceName.iter_mut().zip(LOGON_PROCESS_NAME) {
                *slot = *byte as i8;
            }
            let mut luid = LUID::default();
            // SAFETY: `luid` is an out-parameter owned by this frame.
            unsafe { AllocateLocallyUniqueId(&mut luid) }.map_err(io::Error::other)?;
            source.SourceIdentifier = luid;

            let mut profile: *mut c_void = ptr::null_mut();
            let mut profile_len: u32 = 0;
            let mut logon_id = LUID::default();
            let mut token = HANDLE::default();
            let mut quotas = QUOTA_LIMITS::default();
            let mut sub_status: i32 = 0;
            let local_groups = groups_buffer
                .as_ref()
                .map(|buffer| buffer.as_ptr().cast::<TOKEN_GROUPS>());
            // SAFETY: `buffer` holds the S4U header and the strings it points
            // to; `groups_buffer` holds a TOKEN_GROUPS whose SID pointers
            // borrow `sids`; every out-parameter is owned by this frame and all
            // inputs outlive the call.
            let status = unsafe {
                LsaLogonUser(
                    self.handle,
                    &origin,
                    SECURITY_LOGON_TYPE::Network,
                    package,
                    buffer.as_mut_ptr().cast(),
                    buffer.len() as u32,
                    local_groups,
                    &source,
                    &mut profile,
                    &mut profile_len,
                    &mut logon_id,
                    &mut token,
                    &mut quotas,
                    &mut sub_status,
                )
            };
            if !profile.is_null() {
                // SAFETY: `profile` was allocated by LsaLogonUser for the
                // caller to free, and is not used afterwards.
                let _ = unsafe { LsaFreeReturnBuffer(profile) };
            }
            if status.is_err() {
                return Err(nt_error(status));
            }
            // SAFETY: on success LsaLogonUser hands the caller ownership of a
            // fresh primary token handle; OwnedHandle closes it exactly once.
            let handle = unsafe { OwnedHandle::from_raw_handle(token.0) };
            Ok(super::SessionToken { handle })
        }
    }

    impl Drop for LsaSession {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            // SAFETY: `self.handle` came from LsaRegisterLogonProcess and is
            // released exactly once, here.
            let _ = unsafe { LsaDeregisterLogonProcess(self.handle) };
        }
    }

    // windows-rs exposes these package names as NUL-terminated PCSTRs; the LSA
    // takes counted strings without the terminator.
    const MSV1_0_PACKAGE_NAME_BYTES: &[u8] = b"MICROSOFT_AUTHENTICATION_PACKAGE_V1_0";
    const KERBEROS_NAME_BYTES: &[u8] = b"Kerberos";

    fn unicode(at: *mut u8, units: usize) -> LSA_UNICODE_STRING {
        let bytes = (units * 2) as u16;
        LSA_UNICODE_STRING {
            Length: bytes,
            MaximumLength: bytes,
            Buffer: PWSTR(at.cast()),
        }
    }

    /// Copies `header` into the front of `buffer`.
    ///
    /// The header's string pointers already point further into `buffer`;
    /// writing through the same allocation keeps them valid.
    #[allow(unsafe_code)]
    fn write_header<T: Copy>(buffer: &mut [u8], header: T) {
        assert!(buffer.len() >= size_of::<T>());
        // SAFETY: the buffer holds at least `size_of::<T>()` bytes (asserted),
        // and `write_unaligned` tolerates the Vec<u8> allocation's alignment.
        unsafe { ptr::write_unaligned(buffer.as_mut_ptr().cast::<T>(), header) };
    }

    /// Builds a TOKEN_GROUPS for `sids`, or `None` when there are none.
    ///
    /// The returned buffer holds SID pointers into `sids`, so `sids` must
    /// outlive every use of it.
    fn token_groups(sids: &mut [Vec<u8>]) -> Option<Vec<u64>> {
        if sids.is_empty() {
            return None;
        }
        let bytes = size_of::<TOKEN_GROUPS>() + (sids.len() - 1) * size_of::<SID_AND_ATTRIBUTES>();
        // u64 storage keeps the buffer aligned for TOKEN_GROUPS.
        let mut buffer = vec![0_u64; bytes.div_ceil(8)];
        let groups = buffer.as_mut_ptr().cast::<TOKEN_GROUPS>();
        let attributes = SE_GROUP_MANDATORY | SE_GROUP_ENABLED_BY_DEFAULT | SE_GROUP_ENABLED;
        fill_token_groups(groups, sids, attributes);
        Some(buffer)
    }

    #[allow(unsafe_code)]
    fn fill_token_groups(groups: *mut TOKEN_GROUPS, sids: &mut [Vec<u8>], attributes: u32) {
        // SAFETY: `groups` points to a zeroed, 8-aligned buffer sized for
        // `sids.len()` SID_AND_ATTRIBUTES entries (see `token_groups`), so
        // every write below stays inside it.
        unsafe {
            (*groups).GroupCount = sids.len() as u32;
            let entries = ptr::addr_of_mut!((*groups).Groups).cast::<SID_AND_ATTRIBUTES>();
            for (index, sid) in sids.iter_mut().enumerate() {
                entries.add(index).write(SID_AND_ATTRIBUTES {
                    Sid: PSID(sid.as_mut_ptr().cast()),
                    Attributes: attributes,
                });
            }
        }
    }

    #[allow(unsafe_code)]
    fn set_primary_group(token: &super::SessionToken, sid: &[u8]) -> io::Result<()> {
        let mut sid = sid.to_vec();
        let primary = TOKEN_PRIMARY_GROUP {
            PrimaryGroup: PSID(sid.as_mut_ptr().cast()),
        };
        // SAFETY: `token` is a live primary token; `primary` points to `sid`,
        // which outlives the call.
        unsafe {
            SetTokenInformation(
                HANDLE(token.as_raw_handle()),
                TokenPrimaryGroup,
                ptr::addr_of!(primary).cast(),
                size_of::<TOKEN_PRIMARY_GROUP>() as u32,
            )
        }
        .map_err(io::Error::other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_ops_print_upstreams_call_names() {
        assert_eq!(IdentityOp::Setgid.as_str(), "setgid");
        assert_eq!(IdentityOp::Setgroups.as_str(), "setgroups");
        assert_eq!(IdentityOp::Setuid.as_str(), "setuid");
    }

    // WHY: upstream calls setgid before setuid, so a daemon that cannot switch
    // identity at all must refuse with the call upstream would have hit first.
    #[test]
    fn a_refused_logon_reports_setgid_only_when_groups_were_requested() {
        assert_eq!(first_refused_op(true), IdentityOp::Setgid);
        assert_eq!(first_refused_op(false), IdentityOp::Setuid);
    }

    #[test]
    fn errors_render_like_upstreams_refusals() {
        assert_eq!(
            SessionIdentityError::InvalidUid("bob".into()).to_string(),
            "invalid uid bob"
        );
        assert_eq!(
            SessionIdentityError::InvalidGid("staff".into()).to_string(),
            "invalid gid staff"
        );
        let failed = SessionIdentityError::Failed {
            op: IdentityOp::Setgroups,
            source: io::Error::other("denied"),
        };
        assert_eq!(failed.to_string(), "setgroups failed: denied");
    }

    #[test]
    fn only_a_leading_star_selects_the_account_groups() {
        let star = GroupRequest::parse(["*", "staff"]);
        assert!(star.account_groups);
        assert_eq!(star.names, ["staff"]);
        assert_eq!(star.primary(), None);

        let named = GroupRequest::parse(["staff", "*"]);
        assert!(!named.account_groups);
        assert_eq!(named.names, ["staff", "*"]);
        assert_eq!(named.primary(), Some("staff"));

        assert!(GroupRequest::parse([]).is_empty());
    }

    #[test]
    fn pack_s4u_strings_places_each_string_after_the_header_aligned() {
        let a: Vec<u16> = "ab".encode_utf16().collect();
        let b: Vec<u16> = "c".encode_utf16().collect();
        let (buffer, offsets) = pack_s4u_strings(7, &[&a, &b]);
        assert_eq!(offsets, [8, 12]);
        assert_eq!(buffer.len(), 14);
        assert!(buffer[..8].iter().all(|&byte| byte == 0));
        assert_eq!(&buffer[8..10], &u16::from(b'a').to_ne_bytes());
        assert_eq!(&buffer[10..12], &u16::from(b'b').to_ne_bytes());
        assert_eq!(&buffer[12..14], &u16::from(b'c').to_ne_bytes());
    }

    #[cfg(windows)]
    #[test]
    fn an_unknown_account_is_an_invalid_uid() {
        let error = logon_session_identity("oc-rsync-no-such-user-562", &GroupRequest::default())
            .expect_err("unknown account");
        assert!(
            matches!(error, SessionIdentityError::InvalidUid(ref name) if name == "oc-rsync-no-such-user-562"),
            "{error}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn an_unknown_group_is_an_invalid_gid() {
        let user = std::env::var("USERNAME").expect("USERNAME");
        let groups = GroupRequest::parse(["oc-rsync-no-such-group-562"]);
        let error = logon_session_identity(&user, &groups).expect_err("unknown group");
        assert!(
            matches!(error, SessionIdentityError::InvalidGid(ref name) if name == "oc-rsync-no-such-group-562"),
            "{error}"
        );
    }

    // WHY: CI runs the daemon tests as an ordinary (non-SYSTEM) account; the
    // logon must refuse with upstream's first identity call, never succeed or
    // fall back to the daemon's own identity.
    #[cfg(windows)]
    #[test]
    fn without_tcb_privilege_the_logon_refuses_with_upstreams_first_call() {
        let user = std::env::var("USERNAME").expect("USERNAME");
        let error = logon_session_identity(&user, &GroupRequest::default())
            .expect_err("an unprivileged caller cannot log on another identity");
        assert!(
            matches!(
                error,
                SessionIdentityError::Failed {
                    op: IdentityOp::Setuid,
                    ..
                }
            ),
            "{error}"
        );
    }
}
