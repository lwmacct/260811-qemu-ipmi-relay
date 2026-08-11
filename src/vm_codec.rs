use crate::error::{RelayError, Result};

pub const VM_MSG_CHAR: u8 = 0xa0;
pub const VM_CMD_CHAR: u8 = 0xa1;
pub const VM_ESCAPE_CHAR: u8 = 0xaa;
pub const VM_CMD_VERSION: u8 = 0xff;
pub const VM_CMD_RESET: u8 = 0x04;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpmiMessage {
    pub sequence: u8,
    pub netfn_lun: u8,
    pub command: u8,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Message(IpmiMessage),
    Command(Vec<u8>),
}

pub struct Decoder {
    buf: Vec<u8>,
    escaped: bool,
    too_long: bool,
    max_frame_size: usize,
}

impl Decoder {
    pub fn new(max_frame_size: usize) -> Self {
        Self {
            buf: Vec::new(),
            escaped: false,
            too_long: false,
            max_frame_size,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> Vec<Result<Frame>> {
        let mut frames = Vec::new();
        for &byte in bytes {
            match byte {
                VM_ESCAPE_CHAR if !self.too_long => self.escaped = true,
                VM_MSG_CHAR | VM_CMD_CHAR => {
                    if self.escaped {
                        frames.push(Err(RelayError::Protocol(
                            "frame ended after escape byte".into(),
                        )));
                    } else if self.too_long {
                        frames.push(Err(RelayError::Protocol(
                            "frame exceeds max_frame_size".into(),
                        )));
                    } else if !self.buf.is_empty() {
                        let is_command = byte == VM_CMD_CHAR;
                        frames.push(if is_command {
                            self.decode_command()
                        } else {
                            self.decode_message()
                        });
                    }
                    self.reset();
                }
                byte => {
                    let value = if self.escaped {
                        self.escaped = false;
                        byte & !0x10
                    } else {
                        byte
                    };
                    if self.buf.len() >= self.max_frame_size {
                        self.too_long = true;
                    } else {
                        self.buf.push(value);
                    }
                }
            }
        }
        frames
    }

    fn reset(&mut self) {
        self.buf.clear();
        self.escaped = false;
        self.too_long = false;
    }

    fn decode_command(&self) -> Result<Frame> {
        Ok(Frame::Command(self.buf.clone()))
    }

    fn decode_message(&self) -> Result<Frame> {
        if self.buf.len() < 4 {
            return Err(RelayError::Protocol("IPMI message is too short".into()));
        }
        if checksum(&self.buf) != 0 {
            return Err(RelayError::Protocol(
                "IPMI message checksum mismatch".into(),
            ));
        }
        let end = self.buf.len() - 1;
        Ok(Frame::Message(IpmiMessage {
            sequence: self.buf[0],
            netfn_lun: self.buf[1],
            command: self.buf[2],
            data: self.buf[3..end].to_vec(),
        }))
    }
}

pub fn encode_message(message: &IpmiMessage) -> Vec<u8> {
    let mut raw = Vec::with_capacity(4 + message.data.len());
    raw.extend([message.sequence, message.netfn_lun, message.command]);
    raw.extend(&message.data);
    raw.push(checksum(&raw).wrapping_neg());
    encode_frame(&raw, VM_MSG_CHAR)
}

pub fn encode_command(command: &[u8]) -> Vec<u8> {
    encode_frame(command, VM_CMD_CHAR)
}

fn encode_frame(raw: &[u8], terminator: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + 1);
    for &byte in raw {
        if matches!(byte, VM_MSG_CHAR | VM_CMD_CHAR | VM_ESCAPE_CHAR) {
            out.extend([VM_ESCAPE_CHAR, byte | 0x10]);
        } else {
            out.push(byte);
        }
    }
    out.push(terminator);
    out
}

fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |sum, &byte| sum.wrapping_add(byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_escapes_and_checksum() {
        let expected = IpmiMessage {
            sequence: 7,
            netfn_lun: 0x18,
            command: 0x02,
            data: vec![0xa0, 0xa1, 0xaa, 0x55],
        };
        let encoded = encode_message(&expected);
        let mut decoder = Decoder::new(303);
        let frames = decoder
            .push(&encoded)
            .into_iter()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(frames, vec![Frame::Message(expected)]);
    }

    #[test]
    fn handles_split_and_multiple_frames() {
        let first = encode_command(&[VM_CMD_VERSION, 1]);
        let second = encode_command(&[VM_CMD_RESET]);
        let mut decoder = Decoder::new(303);
        assert!(decoder.push(&first[..1]).is_empty());
        let remainder = first[1..]
            .iter()
            .chain(second.iter())
            .copied()
            .collect::<Vec<_>>();
        let frames = decoder
            .push(&remainder)
            .into_iter()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            frames,
            vec![
                Frame::Command(vec![VM_CMD_VERSION, 1]),
                Frame::Command(vec![VM_CMD_RESET])
            ]
        );
    }

    #[test]
    fn rejects_bad_checksum() {
        let mut decoder = Decoder::new(303);
        let mut frame = encode_message(&IpmiMessage {
            sequence: 1,
            netfn_lun: 0,
            command: 1,
            data: vec![],
        });
        frame[0] ^= 1;
        assert!(decoder.push(&frame).into_iter().next().unwrap().is_err());
    }
}
