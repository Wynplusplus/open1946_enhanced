//! The `traffic` stream of an `.ntrk`: the recorded replication channel.
//!
//! `NetChannelOutStream.sendPacket` writes one packet per send:
//!
//! ```text
//! u16  deltaTime          (ms since last packet; 0 if the gap exceeded 65535)
//! u16  payloadLen         (bytes that follow, = 3 + filtered + guaranted)
//! u16  crc16              (over the bytes from here to the packet end, replacing the
//!                         `7361` magic the writer first placed here)
//! u8   guarantedCount     (number of guaranteed messages)
//!      guarantedCount × message  (the spawn/destroy/guaranted messages)
//!      …filtered messages…       (state updates)
//! ```
//!
//! A guaranteed message (`NetChannel.putMessage`, 3-arg) is:
//!
//! ```text
//! u16  objIndex           (id of the sending object, or a spawn/destroy tag)
//! u8   msgSize            (data length that follows, not counting `iObjects`)
//! msgSize bytes           (the message body)
//! ```
//!
//! `iObjects` (referenced object ids, each `u16`) are appended after the body with no
//! count in the framing — their number is set by the message type when it is built, so
//! the framing reader stops at `msgSize` and leaves the trailing ids to the interpreter.
//!
//! Filtered (state) messages carry a variable-length size+time field; that path is
//! decoded in [`read_filtered_header`]. Spec: `docs/formats/ntrk.md`.

use crate::ntrk::NtrkError;

/// One packet from the `traffic` stream, borrowed into the decoded bytes.
#[derive(Debug)]
pub struct Packet<'a> {
    pub delta_time: u16,
    pub guaranted_count: u8,
    /// Bytes from the CRC field to the packet end: `[crc16: u16][count: u8][messages…]`.
    payload: &'a [u8],
}

impl<'a> Packet<'a> {
    /// Iterates the packets of a decoded `traffic` stream.
    pub fn iter(data: &'a [u8]) -> PacketIter<'a> {
        PacketIter { data, pos: 0 }
    }

    /// Bytes after the 4-byte header and the CRC/count, i.e. the message run.
    pub fn body(&self) -> &'a [u8] {
        &self.payload[3..]
    }
}

pub struct PacketIter<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Iterator for PacketIter<'a> {
    type Item = Result<Packet<'a>, NtrkError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos == self.data.len() {
            return None;
        }
        if self.pos + 4 > self.data.len() {
            return Some(Err(NtrkError::Truncated));
        }
        let dt = u16::from_be_bytes([self.data[self.pos], self.data[self.pos + 1]]);
        let len = u16::from_be_bytes([self.data[self.pos + 2], self.data[self.pos + 3]]) as usize;
        let start = self.pos + 4;
        let end = match start.checked_add(len) {
            Some(e) if e <= self.data.len() => e,
            _ => return Some(Err(NtrkError::Truncated)),
        };
        if len < 3 {
            return Some(Err(NtrkError::Truncated));
        }
        let payload = &self.data[start..end];
        self.pos = end;
        let guaranted_count = payload[2];
        Some(Ok(Packet { delta_time: dt, guaranted_count, payload }))
    }
}

/// One guaranteed message read out of a packet.
#[derive(Debug)]
pub struct GuarantedMsg<'a> {
    pub obj_index: u16,
    pub data: &'a [u8],
}

/// Iterates the guaranteed messages in a packet's [`Packet::body`]. Each is
/// `u16 objIndex, u8 size, size bytes`; the reader stops when fewer than 3 bytes remain.
pub fn guaranted_messages(body: &[u8]) -> GuarantedIter<'_> {
    GuarantedIter { data: body, pos: 0 }
}

pub struct GuarantedIter<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Iterator for GuarantedIter<'a> {
    type Item = Result<GuarantedMsg<'a>, NtrkError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos + 3 > self.data.len() {
            return None;
        }
        let obj_index = u16::from_be_bytes([self.data[self.pos], self.data[self.pos + 1]]);
        let size = self.data[self.pos + 2] as usize;
        let body_start = self.pos + 3;
        let body_end = match body_start.checked_add(size) {
            Some(e) if e <= self.data.len() => e,
            _ => return Some(Err(NtrkError::Truncated)),
        };
        let data = &self.data[body_start..body_end];
        self.pos = body_end;
        Some(Ok(GuarantedMsg { obj_index, data }))
    }
}

/// Decoded header of a filtered (state) message.
#[derive(Debug, PartialEq, Eq)]
pub struct FilteredHeader {
    pub size: u32,
    pub time_delta: Option<i32>,
    pub header_len: usize,
}

/// Parses a filtered-message header starting at `b[0]`. Returns the header; the message
/// body is the next `size` bytes after the `header_len` consumed. The encoding matches
/// `NetChannel.putMessage` (5-arg): the low 5 bits carry either the size (if < 32) or the
/// `32` flag (size in the next byte), and the top two bits carry the time-delta width.
pub fn read_filtered_header(b: &[u8]) -> Result<FilteredHeader, NtrkError> {
    let first = *b.first().ok_or(NtrkError::Truncated)?;
    let time_bits = first & 0xc0;
    if first & 0x20 == 0 {
        // size < 32: the size is the low 5 bits.
        let size = (first & 0x1f) as u32;
        match time_bits {
            0x00 => Ok(FilteredHeader { size, time_delta: None, header_len: 1 }),
            0x40 => {
                let t = *b.get(1).ok_or(NtrkError::Truncated)? as i32;
                let delta = if t & 0x80 != 0 { -(t & 0x7f) } else { t & 0x7f };
                Ok(FilteredHeader { size, time_delta: Some(delta), header_len: 2 })
            }
            0x80 => {
                let b1 = *b.get(1).ok_or(NtrkError::Truncated)? as i32;
                let b2 = *b.get(2).ok_or(NtrkError::Truncated)? as i32;
                let sign = if b1 & 0x80 != 0 { -1 } else { 1 };
                let v = (b1 & 0x7f) << 8 | b2;
                Ok(FilteredHeader { size, time_delta: Some(sign * v), header_len: 3 })
            }
            0xc0 => {
                let b1 = *b.get(1).ok_or(NtrkError::Truncated)? as i32;
                let b2 = *b.get(2).ok_or(NtrkError::Truncated)? as i32;
                let b3 = *b.get(3).ok_or(NtrkError::Truncated)? as i32;
                let sign = if b1 & 0x80 != 0 { -1 } else { 1 };
                let v = (b1 & 0x7f) << 16 | b2 << 8 | b3;
                Ok(FilteredHeader { size, time_delta: Some(sign * v), header_len: 4 })
            }
            _ => unreachable!(),
        }
    } else {
        // size >= 32: bit 5 is the flag, size is in the next byte.
        let size = *b.get(1).ok_or(NtrkError::Truncated)? as u32;
        match time_bits {
            0x00 => Ok(FilteredHeader { size, time_delta: None, header_len: 2 }),
            0x40 => {
                let t = first & 0x0f;
                Ok(FilteredHeader { size, time_delta: Some(t as i32), header_len: 2 })
            }
            0x80 => {
                let b2 = *b.get(2).ok_or(NtrkError::Truncated)? as i32;
                let t = ((first & 0x0f) as i32) << 8 | b2;
                Ok(FilteredHeader { size, time_delta: Some(t), header_len: 3 })
            }
            0xc0 => {
                let b2 = *b.get(2).ok_or(NtrkError::Truncated)? as i32;
                let b3 = *b.get(3).ok_or(NtrkError::Truncated)? as i32;
                let t = ((first & 0x0f) as i32) << 16 | b2 << 8 | b3;
                Ok(FilteredHeader { size, time_delta: Some(t), header_len: 4 })
            }
            _ => unreachable!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ntrk::Container;
    use std::io::Read;

    fn traffic() -> Option<Vec<u8>> {
        let root = std::env::var("IL2_ROOT").ok()?;
        let p = std::path::Path::new(&root).join("records/Intro3b.ntrk");
        let mut f = std::fs::File::open(&p).ok()?;
        let mut v = Vec::new();
        f.read_to_end(&mut v).ok()?;
        let c = Container::open(&v).ok()?;
        c.read("traffic")?.ok()
    }

    /// The whole traffic stream must parse as a clean run of packets with no trailing
    /// bytes and no truncation. (Per-message parsing needs the type registry — each
    /// guaranteed message carries an uncounted run of `iObjects` ids — so this walks the
    /// packet layer only, which the framing fully determines.)
    #[test]
    fn walks_intro3b_traffic_cleanly() {
        let Some(data) = traffic() else { return };
        let mut packets = 0;
        let mut guaranted = 0;
        for pkt in Packet::iter(&data) {
            let pkt = pkt.expect("packet");
            packets += 1;
            guaranted += pkt.guaranted_count as usize;
        }
        assert!(packets > 100, "expected many packets, got {packets}");
        assert!(guaranted > 100);
        eprintln!("intro3b traffic: {packets} packets, {guaranted} guaranted messages");
    }

    #[test]
    fn first_packet_has_known_framing() {
        let Some(data) = traffic() else { return };
        let pkt = Packet::iter(&data).next().unwrap().expect("first packet");
        // Intro3b's first packet carries 13 guaranteed messages (verified by hex dump).
        assert_eq!(pkt.guaranted_count, 13);
    }

    #[test]
    fn filtered_header_no_time() {
        // size 5, no time: first byte = 0b000_00101 = 0x05
        let h = read_filtered_header(&[0x05, 0xaa, 0xbb]).unwrap();
        assert_eq!((h.size, h.time_delta, h.header_len), (5, None, 1));
    }

    #[test]
    fn filtered_header_small_time() {
        // size 5, 1-byte time delta of +3: first byte = 0b010_00101 = 0x45, then 0x03
        let h = read_filtered_header(&[0x45, 0x03]).unwrap();
        assert_eq!((h.size, h.time_delta, h.header_len), (5, Some(3), 2));
    }

    #[test]
    fn filtered_header_extended_size() {
        // size 40 (>= 32): bit 5 set (0x20), size in the next byte; no time.
        let h = read_filtered_header(&[0x20, 0x28]).unwrap();
        assert_eq!((h.size, h.time_delta, h.header_len), (40, None, 2));
    }
}
