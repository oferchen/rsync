use super::*;

pub(super) fn encode_frame(code: MessageCode, payload: &[u8]) -> Vec<u8> {
    let header = MessageHeader::new(code, payload.len() as u32).expect("constructible header");
    let mut bytes = Vec::from(header.encode());
    bytes.extend_from_slice(payload);
    bytes
}
