use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;

#[cfg(all(unix, target_vendor = "apple"))]
use rustix::process;

#[derive(Clone, Debug)]
pub(super) struct UnixProcessIdentity {
    euid: u32,
    egid: u32,
    groups: Vec<u32>,
}

impl UnixProcessIdentity {
    pub(super) fn current() -> Self {
        let euid = nix::unistd::geteuid().as_raw() as u32;
        let egid = nix::unistd::getegid().as_raw() as u32;
        let groups = collect_supplementary_groups();
        Self { euid, egid, groups }
    }

    #[inline]
    pub(super) const fn is_root(&self) -> bool {
        self.euid == 0
    }

    #[inline]
    pub(super) fn in_group(&self, gid: u32) -> bool {
        if self.egid == gid {
            return true;
        }

        self.groups.iter().copied().any(|group| group == gid)
    }
}

#[cfg(test)]
impl UnixProcessIdentity {
    pub(super) fn for_tests(euid: u32, egid: u32, groups: &[u32]) -> Self {
        Self {
            euid,
            egid,
            groups: groups.to_vec(),
        }
    }
}

pub(super) fn unix_can_execute(metadata: &Metadata) -> bool {
    let identity = UnixProcessIdentity::current();
    unix_mode_allows_execution(metadata.mode(), metadata.uid(), metadata.gid(), &identity)
}

pub(super) fn unix_mode_allows_execution(
    mode: u32,
    owner: u32,
    group: u32,
    identity: &UnixProcessIdentity,
) -> bool {
    if mode & 0o111 == 0 {
        return false;
    }

    if identity.is_root() {
        return true;
    }

    if owner == identity.euid {
        return mode & 0o100 != 0;
    }

    if identity.in_group(group) {
        return mode & 0o010 != 0;
    }

    mode & 0o001 != 0
}

#[cfg(all(unix, not(target_vendor = "apple")))]
fn collect_supplementary_groups() -> Vec<u32> {
    match nix::unistd::getgroups() {
        Ok(groups) => groups.into_iter().map(|gid| gid.as_raw()).collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(all(unix, target_vendor = "apple"))]
fn collect_supplementary_groups() -> Vec<u32> {
    match process::getgroups() {
        Ok(groups) => groups.into_iter().map(|gid| gid.as_raw()).collect(),
        Err(_) => Vec::new(),
    }
}
