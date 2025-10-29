use super::*;
use crate::RemoteProtocolAdvertisement;
use crate::binary::{BinaryHandshake, BinaryHandshakeParts, negotiate_binary_session};
use crate::daemon::{
    LegacyDaemonHandshake, LegacyDaemonHandshakeParts, negotiate_legacy_daemon_session,
};
use crate::negotiation::{NEGOTIATION_PROLOGUE_UNDETERMINED_MSG, NegotiatedStream};
use crate::sniff_negotiation_stream;
use rsync_protocol::{
    NegotiationPrologue, NegotiationPrologueSniffer, ProtocolVersion, format_legacy_daemon_greeting,
};
use std::convert::TryFrom;
use std::io::{self, Cursor, Read, Write};

mod basics;
mod clones;
mod mapping;
mod roundtrip;
mod support;

pub(crate) use support::*;
