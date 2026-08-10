//! Bidirectional translation between [`exacl::Perm`] flags and the rsync
//! wire-protocol 3-bit rwx permission mask.

use exacl::Perm;

/// Converts [`exacl::Perm`] flags to rsync 3-bit rwx permission bits.
pub(super) fn exacl_perms_to_rsync(perms: Perm) -> u8 {
    let mut bits: u8 = 0;
    if perms.contains(Perm::READ) {
        bits |= 0x04;
    }
    if perms.contains(Perm::WRITE) {
        bits |= 0x02;
    }
    if perms.contains(Perm::EXECUTE) {
        bits |= 0x01;
    }
    bits
}

/// Converts rsync permission bits (3-bit rwx) to [`exacl::Perm`] flags.
pub(super) fn rsync_perms_to_exacl(bits: u8) -> Perm {
    let mut perms = Perm::empty();
    if bits & 0x04 != 0 {
        perms |= Perm::READ;
    }
    if bits & 0x02 != 0 {
        perms |= Perm::WRITE;
    }
    if bits & 0x01 != 0 {
        perms |= Perm::EXECUTE;
    }
    perms
}
