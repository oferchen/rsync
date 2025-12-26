mod borrowed;
#[cfg(feature = "async")]
mod codec;
mod frame;
mod helpers;
mod io;

#[cfg(test)]
mod tests;

pub use borrowed::{BorrowedMessageFrame, BorrowedMessageFrames};
#[cfg(feature = "async")]
pub use codec::MultiplexCodec;
pub use frame::MessageFrame;
pub use io::{recv_msg, recv_msg_into, send_frame, send_msg};
