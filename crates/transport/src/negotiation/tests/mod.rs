#![cfg(test)]

use super::*;

use std::{
    collections::TryReserveError,
    error::Error as _,
    io::{self, BufRead, Cursor, IoSlice, IoSliceMut, Read, Write},
};

use rsync_protocol::{LEGACY_DAEMON_PREFIX_LEN, ProtocolVersion};

include!("part_01.rs");
include!("part_02.rs");
include!("part_03.rs");
include!("part_04.rs");
include!("part_05.rs");
include!("part_06.rs");
include!("part_07.rs");
