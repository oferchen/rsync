use super::*;

use std::{
    collections::TryReserveError,
    error::Error as _,
    io::{self, BufRead, Cursor, IoSlice, IoSliceMut, Read, Write},
};

use rsync_protocol::{
    LEGACY_DAEMON_PREFIX_LEN, LegacyDaemonMessage, NegotiationPrologue, NegotiationPrologueSniffer,
    ProtocolVersion,
};

include!("chunk_00.rs");
include!("chunk_01.rs");
include!("chunk_02.rs");
include!("chunk_03.rs");
include!("chunk_04.rs");
include!("chunk_05.rs");
include!("chunk_06.rs");
