use super::{
    BorrowedMessageFrame, BorrowedMessageFrames, MessageFrame,
    helpers::{ensure_payload_length, reserve_payload},
    recv_msg, recv_msg_into, send_frame, send_msg,
};
use crate::envelope::{HEADER_LEN, MAX_PAYLOAD_LENGTH, MPLEX_BASE, MessageCode, MessageHeader};

mod borrowed;
mod frame;
mod limits;
mod receive;
mod send;
mod support;
