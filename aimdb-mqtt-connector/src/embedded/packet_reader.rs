//! Incremental MQTT packet framing: push bytes in, take whole packets out.
//!
//! This is what makes a partial packet a non-event. `mountain-mqtt`'s own
//! reader asks the transport for exactly as many bytes as the fixed header
//! promises and waits inside that read until they arrive, so a peer that
//! stalls mid-packet parks the caller — with the polled session loop that
//! meant pings, liveness and every queued publish stopped with it. Here a
//! partial packet is simply "not enough yet": [`feed`](PacketReader::feed)
//! takes whatever arrived, [`framed_len`](PacketReader::framed_len) says
//! whether a whole packet is present, and nothing blocks.
//!
//! # Why framing and parsing are separate
//!
//! `framed_len` borrows nothing and [`parse`](PacketReader::parse) takes
//! `&self`, because `MqttBufReader` holds `&[u8]` rather than `&mut [u8]`. So
//! a parsed packet holds a *shared* borrow of the buffer, it ends when the
//! caller drops the packet, and [`consume`](PacketReader::consume) is then
//! free to take `&mut self` and compact. No `unsafe`, no self-referential
//! struct, no allocation per packet.

use mountain_mqtt::codec::mqtt_reader::{MqttBufReader, MqttReader};
use mountain_mqtt::data::packet_type::PacketType;
use mountain_mqtt::error::PacketReadError;
use mountain_mqtt::packets::packet_generic::PacketGeneric;

/// Reassembles MQTT packets from arbitrary byte chunks.
///
/// `N` bounds the largest packet that can be received; a longer one is
/// [`PacketReadError::PacketTooLargeForBuffer`] rather than a stall.
pub(crate) struct PacketReader<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> PacketReader<N> {
    /// An empty reader.
    pub(crate) const fn new() -> Self {
        Self {
            buf: [0u8; N],
            len: 0,
        }
    }

    /// Append freshly read bytes.
    ///
    /// Fails only if they would not fit, which at this layer means the peer
    /// sent a packet larger than `N`.
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> Result<(), PacketReadError> {
        if self.len + bytes.len() > N {
            return Err(PacketReadError::PacketTooLargeForBuffer);
        }
        self.buf[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
        Ok(())
    }

    /// Total length of the complete packet at the head of the buffer, or
    /// `Ok(None)` if not enough bytes have landed yet.
    ///
    /// This is upstream's `receive_rest_of_packet` varint scan restated as a
    /// pure function over what is already buffered, so it borrows nothing and
    /// commits to nothing.
    pub(crate) fn framed_len(&self) -> Result<Option<usize>, PacketReadError> {
        if self.len < 1 {
            return Ok(None);
        }
        if !PacketType::is_valid_first_header_byte(self.buf[0]) {
            return Err(PacketReadError::InvalidPacketType);
        }

        // The remaining-length field: up to 4 bytes, each continuing while its
        // top bit is set.
        let mut pos = 1usize;
        loop {
            if pos > 4 {
                return Err(PacketReadError::InvalidVariableByteIntegerEncoding);
            }
            if self.len < pos + 1 {
                return Ok(None); // the length itself is still arriving
            }
            if self.buf[pos] & 128 == 0 {
                pos += 1;
                break;
            }
            pos += 1;
        }

        let remaining = {
            let mut reader = MqttBufReader::new(&self.buf[1..pos]);
            reader.get_variable_u32()? as usize
        };
        let total = pos + remaining;
        if total > N {
            return Err(PacketReadError::PacketTooLargeForBuffer);
        }
        if self.len < total {
            return Ok(None); // header complete, body still arriving
        }
        Ok(Some(total))
    }

    /// Parse the complete packet at the head of the buffer.
    ///
    /// `total` must come from [`framed_len`](Self::framed_len). Takes `&self`,
    /// so the returned packet holds only a shared borrow — see the module note.
    pub(crate) fn parse<const P: usize, const W: usize, const S: usize>(
        &self,
        total: usize,
    ) -> Result<PacketGeneric<'_, P, W, S>, PacketReadError> {
        let mut reader = MqttBufReader::new(&self.buf[0..total]);
        reader.get()
    }

    /// Drop a consumed packet from the head, sliding any bytes of the next one
    /// down. Needs `&mut self`, so it can only run once the packet is dropped.
    pub(crate) fn consume(&mut self, total: usize) {
        self.buf.copy_within(total..self.len, 0);
        self.len -= total;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    /// MQTT 5 CONNACK, as the fake broker in `tests/common` sends it.
    const CONNACK: &[u8] = &[0x20, 0x03, 0x00, 0x00, 0x00];

    /// A QoS-0 MQTT 5 PUBLISH, built the way `tests/common::publish` builds it.
    fn publish_bytes(topic: &str, payload: &[u8]) -> Vec<u8> {
        let mut rest = Vec::new();
        rest.extend_from_slice(&(topic.len() as u16).to_be_bytes());
        rest.extend_from_slice(topic.as_bytes());
        rest.push(0x00); // no properties
        rest.extend_from_slice(payload);

        let mut packet = vec![0x30];
        let mut n = rest.len();
        loop {
            let mut byte = (n % 128) as u8;
            n /= 128;
            if n > 0 {
                byte |= 128;
            }
            packet.push(byte);
            if n == 0 {
                break;
            }
        }
        packet.extend_from_slice(&rest);
        packet
    }

    /// Drain whatever is complete, reporting each packet as a short label.
    fn drain<const N: usize>(reader: &mut PacketReader<N>) -> Vec<&'static str> {
        let mut got = Vec::new();
        while let Some(total) = reader.framed_len().expect("framing must not error") {
            {
                let packet = reader.parse::<8, 0, 0>(total).expect("parse");
                got.push(match packet {
                    PacketGeneric::Connack(_) => "connack",
                    PacketGeneric::Publish(_) => "publish",
                    _ => "other",
                });
                // the packet drops here, releasing the shared borrow...
            }
            // ...so `consume` can take `&mut self`.
            reader.consume(total);
        }
        got
    }

    #[test]
    fn one_byte_at_a_time_both_packets_parse() {
        let mut wire = Vec::new();
        wire.extend_from_slice(CONNACK);
        wire.extend_from_slice(&publish_bytes("sensor/temp", b"21.5"));

        let mut reader = PacketReader::<256>::new();
        let mut got = Vec::new();
        for byte in &wire {
            reader.feed(&[*byte]).expect("feed");
            got.extend(drain(&mut reader));
        }

        assert_eq!(got, vec!["connack", "publish"]);
        assert_eq!(reader.len, 0, "buffer fully drained");
    }

    #[test]
    fn coalesced_chunk_yields_both_packets() {
        let mut wire = Vec::new();
        wire.extend_from_slice(CONNACK);
        wire.extend_from_slice(&publish_bytes("a/b", b"x"));

        let mut reader = PacketReader::<256>::new();
        reader.feed(&wire).expect("feed");

        assert_eq!(drain(&mut reader), vec!["connack", "publish"]);
        assert_eq!(reader.len, 0);
    }

    #[test]
    fn every_partial_packet_reads_as_incomplete() {
        let bytes = publish_bytes("sensor/temp", b"21.5");
        let mut reader = PacketReader::<256>::new();

        for cut in 1..bytes.len() {
            reader.len = 0;
            reader.feed(&bytes[..cut]).expect("feed prefix");
            assert_eq!(
                reader.framed_len().expect("a prefix must not error"),
                None,
                "a strict prefix must read as incomplete, not as a packet"
            );
        }

        reader.len = 0;
        reader.feed(&bytes).expect("feed whole");
        assert_eq!(reader.framed_len().expect("framing"), Some(bytes.len()));
    }

    #[test]
    fn two_byte_varint_length_reassembles() {
        let bytes = publish_bytes("t", &vec![b'z'; 300]);
        assert!(bytes[1] & 128 != 0, "length needs two varint bytes");

        let mut reader = PacketReader::<512>::new();
        let mut got = Vec::new();
        for byte in &bytes {
            reader.feed(&[*byte]).expect("feed");
            got.extend(drain(&mut reader));
        }

        assert_eq!(got, vec!["publish"]);
    }

    #[test]
    fn a_packet_larger_than_the_buffer_errors_rather_than_stalling() {
        let bytes = publish_bytes("t", &vec![b'z'; 300]);
        let mut reader = PacketReader::<64>::new();

        // The header alone is enough to know it will never fit.
        reader.feed(&bytes[..4]).expect("feed header");
        assert_eq!(
            reader.framed_len(),
            Err(PacketReadError::PacketTooLargeForBuffer)
        );
    }

    #[test]
    fn a_bad_first_header_byte_errors_immediately() {
        let mut reader = PacketReader::<64>::new();
        reader.feed(&[0x00]).expect("feed");
        assert_eq!(reader.framed_len(), Err(PacketReadError::InvalidPacketType));
    }
}
