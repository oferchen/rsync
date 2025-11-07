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

include!("legacy_negotiation_test_support.rs");
include!("legacy_negotiation_sniffer_tests.rs");
include!("negotiated_stream_buffer_tests.rs");
include!("negotiated_stream_parts_tests.rs");
include!("negotiated_stream_clone_tests.rs");
include!("negotiation_error_handling_tests.rs");
include!("legacy_daemon_message_tests.rs");
