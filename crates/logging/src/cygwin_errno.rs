//! Cygwin's Windows-error-to-errno mapping and `strerror` text.
//!
//! On Windows the reference is rsync 3.5.1 built under Cygwin, which turns a
//! Win32 or Winsock error into a Cygwin errno and prints `strerror(errno)`.
//! Generated from newlib-cygwin b5a7119: `winsup/cygwin/local_includes/errmap.h`
//! (Win32 codes), `winsup/cygwin/net.cc` `wsock_errmap` (Winsock codes) and
//! `winsup/cygwin/errno.cc` `_sys_errlist` (text).

/// Winsock error codes start here; `net.cc` `find_winsock_errno()` indexes
/// `wsock_errmap` by `code - WSABASEERR`.
const WSABASEERR: u32 = 10_000;

/// Cygwin's errno for a Win32 or Winsock error code.
///
/// An unmapped code falls back to `EACCES` (13), as both
/// `seterrno_from_win_error()` (errno.cc:207) and `find_winsock_errno()`
/// (net.cc:229) do.
pub(crate) fn errno_from_win_error(code: u32) -> i32 {
    let mapped = if code >= WSABASEERR {
        winsock_errno(code)
    } else {
        win32_errno(code)
    };
    mapped.unwrap_or(EACCES)
}

const EACCES: i32 = 13;

/// upstream: newlib-cygwin errmap.h - non-zero entries only.
fn win32_errno(code: u32) -> Option<i32> {
    Some(match code {
        1 => 54, // ERROR_INVALID_FUNCTION -> EBADRQC
        2 => 2, // ERROR_FILE_NOT_FOUND -> ENOENT
        3 => 2, // ERROR_PATH_NOT_FOUND -> ENOENT
        4 => 24, // ERROR_TOO_MANY_OPEN_FILES -> EMFILE
        5 => 13, // ERROR_ACCESS_DENIED -> EACCES
        6 => 9, // ERROR_INVALID_HANDLE -> EBADF
        8 => 12, // ERROR_NOT_ENOUGH_MEMORY -> ENOMEM
        13 => 22, // ERROR_INVALID_DATA -> EINVAL
        14 => 12, // ERROR_OUTOFMEMORY -> ENOMEM
        15 => 19, // ERROR_INVALID_DRIVE -> ENODEV
        17 => 18, // ERROR_NOT_SAME_DEVICE -> EXDEV
        18 => 89, // ERROR_NO_MORE_FILES -> ENMFILE
        19 => 30, // ERROR_WRITE_PROTECT -> EROFS
        20 => 19, // ERROR_BAD_UNIT -> ENODEV
        21 => 135, // ERROR_NOT_READY -> ENOMEDIUM
        23 => 5, // ERROR_CRC -> EIO
        25 => 22, // ERROR_SEEK -> EINVAL
        27 => 22, // ERROR_SECTOR_NOT_FOUND -> EINVAL
        32 => 16, // ERROR_SHARING_VIOLATION -> EBUSY
        33 => 16, // ERROR_LOCK_VIOLATION -> EBUSY
        36 => 46, // ERROR_SHARING_BUFFER_EXCEEDED -> ENOLCK
        38 => 61, // ERROR_HANDLE_EOF -> ENODATA
        39 => 28, // ERROR_HANDLE_DISK_FULL -> ENOSPC
        50 => 88, // ERROR_NOT_SUPPORTED -> ENOSYS
        51 => 64, // ERROR_REM_NOT_LIST -> ENONET
        52 => 80, // ERROR_DUP_NAME -> ENOTUNIQ
        53 => 2, // ERROR_BAD_NETPATH -> ENOENT
        55 => 2, // ERROR_DEV_NOT_EXIST -> ENOENT
        58 => 88, // ERROR_BAD_NET_RESP -> ENOSYS
        59 => 5, // ERROR_UNEXP_NET_ERR -> EIO
        64 => 2, // ERROR_NETNAME_DELETED -> ENOENT
        67 => 2, // ERROR_BAD_NET_NAME -> ENOENT
        80 => 17, // ERROR_FILE_EXISTS -> EEXIST
        82 => 1, // ERROR_CANNOT_MAKE -> EPERM
        87 => 22, // ERROR_INVALID_PARAMETER -> EINVAL
        89 => 11, // ERROR_NO_PROC_SLOTS -> EAGAIN
        104 => 4, // ERROR_INVALID_AT_INTERRUPT_TIME -> EINTR
        109 => 32, // ERROR_BROKEN_PIPE -> EPIPE
        110 => 5, // ERROR_OPEN_FAILED -> EIO
        112 => 28, // ERROR_DISK_FULL -> ENOSPC
        113 => 23, // ERROR_NO_MORE_SEARCH_HANDLES -> ENFILE
        120 => 88, // ERROR_CALL_NOT_IMPLEMENTED -> ENOSYS
        123 => 2, // ERROR_INVALID_NAME -> ENOENT
        126 => 2, // ERROR_MOD_NOT_FOUND -> ENOENT
        127 => 3, // ERROR_PROC_NOT_FOUND -> ESRCH
        128 => 10, // ERROR_WAIT_NO_CHILDREN -> ECHILD
        129 => 16, // ERROR_CHILD_NOT_COMPLETE -> EBUSY
        131 => 22, // ERROR_NEGATIVE_SEEK -> EINVAL
        145 => 90, // ERROR_DIR_NOT_EMPTY -> ENOTEMPTY
        156 => 5, // ERROR_SIGNAL_REFUSED -> EIO
        161 => 2, // ERROR_BAD_PATHNAME -> ENOENT
        162 => 16, // ERROR_SIGNAL_PENDING -> EBUSY
        164 => 11, // ERROR_MAX_THRDS_REACHED -> EAGAIN
        170 => 16, // ERROR_BUSY -> EBUSY
        183 => 17, // ERROR_ALREADY_EXISTS -> EEXIST
        191 => 8, // ERROR_INVALID_EXE_SIGNATURE -> ENOEXEC
        192 => 8, // ERROR_EXE_MARKED_INVALID -> ENOEXEC
        193 => 8, // ERROR_BAD_EXE_FORMAT -> ENOEXEC
        197 => 8, // ERROR_IOPL_NOT_ENABLED -> ENOEXEC
        205 => 5, // ERROR_NO_SIGNAL_SENT -> EIO
        206 => 91, // ERROR_FILENAME_EXCED_RANGE -> ENAMETOOLONG
        208 => 22, // ERROR_META_EXPANSION_TOO_LONG -> EINVAL
        209 => 22, // ERROR_INVALID_SIGNAL_NUMBER -> EINVAL
        210 => 22, // ERROR_THREAD_1_INACTIVE -> EINVAL
        216 => 8, // ERROR_EXE_MACHINE_TYPE_MISMATCH -> ENOEXEC
        230 => 22, // ERROR_BAD_PIPE -> EINVAL
        231 => 16, // ERROR_PIPE_BUSY -> EBUSY
        232 => 32, // ERROR_NO_DATA -> EPIPE
        233 => 70, // ERROR_PIPE_NOT_CONNECTED -> ECOMM
        234 => 122, // ERROR_MORE_DATA -> EMSGSIZE
        254 => 22, // ERROR_INVALID_EA_NAME -> EINVAL
        255 => 22, // ERROR_EA_LIST_INCONSISTENT -> EINVAL
        259 => 89, // ERROR_NO_MORE_ITEMS -> ENMFILE
        267 => 20, // ERROR_DIRECTORY -> ENOTDIR
        275 => 28, // ERROR_EAS_DIDNT_FIT -> ENOSPC
        277 => 28, // ERROR_EA_TABLE_FULL -> ENOSPC
        282 => 134, // ERROR_EAS_NOT_SUPPORTED -> ENOTSUP
        288 => 1, // ERROR_NOT_OWNER -> EPERM
        424 => 22, // ERROR_CASE_DIFFERING_NAMES_IN_DIR -> EINVAL
        433 => 19, // ERROR_NO_SUCH_DEVICE -> ENODEV
        487 => 22, // ERROR_INVALID_ADDRESS -> EINVAL
        535 => 16, // ERROR_PIPE_CONNECTED -> EBUSY
        536 => 70, // ERROR_PIPE_LISTENING -> ECOMM
        996 => 11, // ERROR_IO_INCOMPLETE -> EAGAIN
        997 => 11, // ERROR_IO_PENDING -> EAGAIN
        998 => 14, // ERROR_NOACCESS -> EFAULT
        1006 => 6, // ERROR_FILE_INVALID -> ENXIO
        1008 => 22, // ERROR_NO_TOKEN -> EINVAL
        1053 => 16, // ERROR_SERVICE_REQUEST_TIMEOUT -> EBUSY
        1067 => 14, // ERROR_PROCESS_ABORTED -> EFAULT
        1100 => 28, // ERROR_END_OF_MEDIA -> ENOSPC
        1101 => 5, // ERROR_FILEMARK_DETECTED -> EIO
        1102 => 5, // ERROR_BEGINNING_OF_MEDIA -> EIO
        1103 => 5, // ERROR_SETMARK_DETECTED -> EIO
        1104 => 5, // ERROR_NO_DATA_DETECTED -> EIO
        1106 => 5, // ERROR_INVALID_BLOCK_LENGTH -> EIO
        1110 => 19, // ERROR_MEDIA_CHANGED -> ENODEV
        1111 => 5, // ERROR_BUS_RESET -> EIO
        1112 => 135, // ERROR_NO_MEDIA_IN_DRIVE -> ENOMEDIUM
        1117 => 5, // ERROR_IO_DEVICE -> EIO
        1129 => 5, // ERROR_EOM_OVERFLOW -> EIO
        1131 => 56, // ERROR_POSSIBLE_DEADLOCK -> EDEADLOCK
        1142 => 31, // ERROR_TOO_MANY_LINKS -> EMLINK
        1165 => 5, // ERROR_DEVICE_REQUIRES_CLEANING -> EIO
        1166 => 5, // ERROR_DEVICE_DOOR_OPEN -> EIO
        1200 => 19, // ERROR_BAD_DEVICE -> ENODEV
        1223 => 4, // ERROR_CANCELLED -> EINTR
        1225 => 111, // ERROR_CONNECTION_REFUSED -> ECONNREFUSED
        1295 => 27, // ERROR_DISK_QUOTA_EXCEEDED -> EFBIG
        1314 => 1, // ERROR_PRIVILEGE_NOT_HELD -> EPERM
        1332 => 22, // ERROR_NONE_MAPPED -> EINVAL
        1392 => 17, // ERROR_FILE_CORRUPT -> EEXIST
        1393 => 5, // ERROR_DISK_CORRUPT -> EIO
        1450 => 27, // ERROR_NO_SYSTEM_RESOURCES -> EFBIG
        1451 => 11, // ERROR_NONPAGED_SYSTEM_RESOURCES -> EAGAIN
        1452 => 11, // ERROR_PAGED_SYSTEM_RESOURCES -> EAGAIN
        1453 => 11, // ERROR_WORKING_SET_QUOTA -> EAGAIN
        1454 => 11, // ERROR_PAGEFILE_QUOTA -> EAGAIN
        1455 => 11, // ERROR_COMMITMENT_LIMIT -> EAGAIN
        1460 => 16, // ERROR_TIMEOUT -> EBUSY
        1816 => 11, // ERROR_NOT_ENOUGH_QUOTA -> EAGAIN
        2202 => 22, // ERROR_BAD_USERNAME -> EINVAL
        2250 => 67, // ERROR_NOT_CONNECTED -> ENOLINK
        2401 => 11, // ERROR_OPEN_FILES -> EAGAIN
        2402 => 11, // ERROR_ACTIVE_CONNECTIONS -> EAGAIN
        2404 => 11, // ERROR_DEVICE_IN_USE -> EAGAIN
        8341 => 5, // ERROR_DS_GENERIC_ERROR -> EIO
        _ => return None,
    })
}

/// upstream: newlib-cygwin net.cc `wsock_errmap` - non-zero entries only.
fn winsock_errno(code: u32) -> Option<i32> {
    Some(match code {
        10004 => 4, // WSAEINTR -> EINTR
        10009 => 9, // WSAEBADF -> EBADF
        10013 => 13, // WSAEACCES -> EACCES
        10014 => 14, // WSAEFAULT -> EFAULT
        10022 => 22, // WSAEINVAL -> EINVAL
        10024 => 24, // WSAEMFILE -> EMFILE
        10035 => 11, // WSAEWOULDBLOCK -> EWOULDBLOCK
        10036 => 119, // WSAEINPROGRESS -> EINPROGRESS
        10037 => 120, // WSAEALREADY -> EALREADY
        10038 => 108, // WSAENOTSOCK -> ENOTSOCK
        10039 => 121, // WSAEDESTADDRREQ -> EDESTADDRREQ
        10040 => 122, // WSAEMSGSIZE -> EMSGSIZE
        10041 => 107, // WSAEPROTOTYPE -> EPROTOTYPE
        10042 => 109, // WSAENOPROTOOPT -> ENOPROTOOPT
        10043 => 123, // WSAEPROTONOSUPPORT -> EPROTONOSUPPORT
        10044 => 124, // WSAESOCKTNOSUPPORT -> ESOCKTNOSUPPORT
        10045 => 95, // WSAEOPNOTSUPP -> EOPNOTSUPP
        10046 => 96, // WSAEPFNOSUPPORT -> EPFNOSUPPORT
        10047 => 106, // WSAEAFNOSUPPORT -> EAFNOSUPPORT
        10048 => 112, // WSAEADDRINUSE -> EADDRINUSE
        10049 => 125, // WSAEADDRNOTAVAIL -> EADDRNOTAVAIL
        10050 => 115, // WSAENETDOWN -> ENETDOWN
        10051 => 114, // WSAENETUNREACH -> ENETUNREACH
        10052 => 126, // WSAENETRESET -> ENETRESET
        10053 => 113, // WSAECONNABORTED -> ECONNABORTED
        10054 => 104, // WSAECONNRESET -> ECONNRESET
        10055 => 105, // WSAENOBUFS -> ENOBUFS
        10056 => 127, // WSAEISCONN -> EISCONN
        10057 => 128, // WSAENOTCONN -> ENOTCONN
        10058 => 110, // WSAESHUTDOWN -> ESHUTDOWN
        10059 => 129, // WSAETOOMANYREFS -> ETOOMANYREFS
        10060 => 116, // WSAETIMEDOUT -> ETIMEDOUT
        10061 => 111, // WSAECONNREFUSED -> ECONNREFUSED
        10062 => 92, // WSAELOOP -> ELOOP
        10063 => 91, // WSAENAMETOOLONG -> ENAMETOOLONG
        10064 => 117, // WSAEHOSTDOWN -> EHOSTDOWN
        10065 => 118, // WSAEHOSTUNREACH -> EHOSTUNREACH
        10066 => 90, // WSAENOTEMPTY -> ENOTEMPTY
        10067 => 130, // WSAEPROCLIM -> EPROCLIM
        10068 => 131, // WSAEUSERS -> EUSERS
        10069 => 132, // WSAEDQUOT -> EDQUOT
        10070 => 133, // WSAESTALE -> ESTALE
        10071 => 66, // WSAEREMOTE -> EREMOTE
        _ => return None,
    })
}

/// Cygwin's `strerror(errnum)`: the `_sys_errlist` entry, or
/// `Unknown error N` for a gap or an out-of-range number
/// (errno.cc:229-256 `strerror_worker()` / `_strerror_r()`).
pub(crate) fn strerror(errnum: i32) -> String {
    let text = match errnum {
        0 => "No error",
        1 => "Operation not permitted",
        2 => "No such file or directory",
        3 => "No such process",
        4 => "Interrupted system call",
        5 => "Input/output error",
        6 => "No such device or address",
        7 => "Argument list too long",
        8 => "Exec format error",
        9 => "Bad file descriptor",
        10 => "No child processes",
        11 => "Resource temporarily unavailable",
        12 => "Cannot allocate memory",
        13 => "Permission denied",
        14 => "Bad address",
        15 => "Block device required",
        16 => "Device or resource busy",
        17 => "File exists",
        18 => "Invalid cross-device link",
        19 => "No such device",
        20 => "Not a directory",
        21 => "Is a directory",
        22 => "Invalid argument",
        23 => "Too many open files in system",
        24 => "Too many open files",
        25 => "Inappropriate ioctl for device",
        26 => "Text file busy",
        27 => "File too large",
        28 => "No space left on device",
        29 => "Illegal seek",
        30 => "Read-only file system",
        31 => "Too many links",
        32 => "Broken pipe",
        33 => "Numerical argument out of domain",
        34 => "Numerical result out of range",
        35 => "No message of desired type",
        36 => "Identifier removed",
        37 => "Channel number out of range",
        38 => "Level 2 not synchronized",
        39 => "Level 3 halted",
        40 => "Level 3 reset",
        41 => "Link number out of range",
        42 => "Protocol driver not attached",
        43 => "No CSI structure available",
        44 => "Level 2 halted",
        45 => "Resource deadlock avoided",
        46 => "No locks available",
        50 => "Invalid exchange",
        51 => "Invalid request descriptor",
        52 => "Exchange full",
        53 => "No anode",
        54 => "Invalid request code",
        55 => "Invalid slot",
        56 => "File locking deadlock error",
        57 => "Bad font file format",
        60 => "Device not a stream",
        61 => "No data available",
        62 => "Timer expired",
        63 => "Out of streams resources",
        64 => "Machine is not on the network",
        65 => "Package not installed",
        66 => "Object is remote",
        67 => "Link has been severed",
        68 => "Advertise error",
        69 => "Srmount error",
        70 => "Communication error on send",
        71 => "Protocol error",
        74 => "Multihop attempted",
        75 => "Inode is remote (not really error)",
        76 => "RFS specific error",
        77 => "Bad message",
        79 => "Inappropriate file type or format",
        80 => "Name not unique on network",
        81 => "File descriptor in bad state",
        82 => "Remote address changed",
        83 => "Can not access a needed shared library",
        84 => "Accessing a corrupted shared library",
        85 => ".lib section in a.out corrupted",
        86 => "Attempting to link in too many shared libraries",
        87 => "Cannot exec a shared library directly",
        88 => "Function not implemented",
        89 => "No more files",
        90 => "Directory not empty",
        91 => "File name too long",
        92 => "Too many levels of symbolic links",
        95 => "Operation not supported",
        96 => "Protocol family not supported",
        104 => "Connection reset by peer",
        105 => "No buffer space available",
        106 => "Address family not supported by protocol",
        107 => "Protocol wrong type for socket",
        108 => "Socket operation on non-socket",
        109 => "Protocol not available",
        110 => "Cannot send after transport endpoint shutdown",
        111 => "Connection refused",
        112 => "Address already in use",
        113 => "Software caused connection abort",
        114 => "Network is unreachable",
        115 => "Network is down",
        116 => "Connection timed out",
        117 => "Host is down",
        118 => "No route to host",
        119 => "Operation now in progress",
        120 => "Operation already in progress",
        121 => "Destination address required",
        122 => "Message too long",
        123 => "Protocol not supported",
        124 => "Socket type not supported",
        125 => "Cannot assign requested address",
        126 => "Network dropped connection on reset",
        127 => "Transport endpoint is already connected",
        128 => "Transport endpoint is not connected",
        129 => "Too many references: cannot splice",
        130 => "Too many processes",
        131 => "Too many users",
        132 => "Disk quota exceeded",
        133 => "Stale NFS file handle",
        134 => "Not supported",
        135 => "No medium found",
        138 => "Invalid or incomplete multibyte or wide character",
        139 => "Value too large for defined data type",
        140 => "Operation canceled",
        141 => "State not recoverable",
        _ => return format!("Unknown error {errnum}"),
    };
    text.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win32_codes_map_like_errmap_h() {
        assert_eq!(errno_from_win_error(2), 2); // ERROR_FILE_NOT_FOUND -> ENOENT
        assert_eq!(errno_from_win_error(5), 13); // ERROR_ACCESS_DENIED -> EACCES
        assert_eq!(errno_from_win_error(112), 28); // ERROR_DISK_FULL -> ENOSPC
        assert_eq!(errno_from_win_error(145), 90); // ERROR_DIR_NOT_EMPTY -> ENOTEMPTY
    }

    #[test]
    fn winsock_codes_map_like_wsock_errmap() {
        assert_eq!(errno_from_win_error(10035), 11); // WSAEWOULDBLOCK -> EAGAIN
        assert_eq!(errno_from_win_error(10054), 104); // WSAECONNRESET
        assert_eq!(errno_from_win_error(10061), 111); // WSAECONNREFUSED
    }

    #[test]
    fn an_unmapped_code_falls_back_to_eacces() {
        assert_eq!(errno_from_win_error(7), 13); // ERROR_ARENA_TRASHED is 0 in errmap.h
        assert_eq!(errno_from_win_error(10001), 13);
        assert_eq!(errno_from_win_error(u32::MAX), 13);
    }

    #[test]
    fn strerror_uses_cygwin_text_and_numbering() {
        assert_eq!(strerror(2), "No such file or directory");
        assert_eq!(strerror(92), "Too many levels of symbolic links");
        assert_eq!(strerror(47), "Unknown error 47");
        assert_eq!(strerror(136), "Unknown error 136");
        assert_eq!(strerror(-1), "Unknown error -1");
    }
}
