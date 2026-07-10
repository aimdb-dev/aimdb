//! SNTPv4 wire format (RFC 4330 subset) — pure encode/parse, no I/O.
//!
//! Feature-independent so the codec is unit-tested on the host; the Embassy
//! I/O task around it lives in [`sntp`](crate::sntp) (`embassy-tls` only).

/// Seconds between the NTP epoch (1900-01-01) and the Unix epoch (1970-01-01).
const NTP_UNIX_OFFSET: u64 = 2_208_988_800;

/// SNTP packets are exactly 48 bytes (no authenticator).
pub(crate) const PACKET_SIZE: usize = 48;

/// A client request: LI = 0, VN = 4, Mode = 3 (client), everything else zero.
/// (RFC 4330 §5: a client may leave all timestamps zero and use only the
/// reply's transmit timestamp.)
pub(crate) fn request() -> [u8; PACKET_SIZE] {
    let mut packet = [0u8; PACKET_SIZE];
    packet[0] = 0x23; // 00_100_011: LI 0, VN 4, Mode 3
    packet
}

/// Parse a server reply into Unix seconds, or `None` if the packet is not a
/// plausible time: wrong size/mode, unsynchronized server (LI = 3), invalid
/// stratum (0 is a kiss-of-death, > 15 is reserved), or a zero/pre-Unix-epoch
/// transmit timestamp. Sub-second fraction is dropped — certificate validity
/// has day granularity.
///
/// The 32-bit seconds field wraps in 2036 (NTP era 1); like every plain SNTP
/// consumer this treats all timestamps as era 0.
pub(crate) fn parse_reply(packet: &[u8]) -> Option<u64> {
    if packet.len() < PACKET_SIZE {
        return None;
    }
    let leap = packet[0] >> 6;
    let mode = packet[0] & 0x07;
    let stratum = packet[1];
    if leap == 3 || mode != 4 || stratum == 0 || stratum > 15 {
        return None;
    }
    // Transmit timestamp, seconds part (bytes 40..44).
    let ntp_secs = u64::from(u32::from_be_bytes([
        packet[40], packet[41], packet[42], packet[43],
    ]));
    if ntp_secs == 0 {
        return None;
    }
    ntp_secs.checked_sub(NTP_UNIX_OFFSET)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply_with(transmit_secs: u32) -> [u8; PACKET_SIZE] {
        let mut packet = [0u8; PACKET_SIZE];
        packet[0] = 0x24; // LI 0, VN 4, Mode 4 (server)
        packet[1] = 2; // stratum 2
        packet[40..44].copy_from_slice(&transmit_secs.to_be_bytes());
        packet
    }

    #[test]
    fn request_header() {
        let packet = request();
        assert_eq!(packet[0], 0x23);
        assert!(packet[1..].iter().all(|&b| b == 0));
    }

    #[test]
    fn parses_valid_reply() {
        // 2026-01-01T00:00:00Z = Unix 1767225600 = NTP 3976214400
        assert_eq!(parse_reply(&reply_with(3_976_214_400)), Some(1_767_225_600));
    }

    #[test]
    fn rejects_bad_packets() {
        assert_eq!(parse_reply(&[0u8; 40]), None); // truncated
        let mut p = reply_with(3_976_214_400);
        p[0] = 0x23; // mode 3 (client) echoed back
        assert_eq!(parse_reply(&p), None);
        let mut p = reply_with(3_976_214_400);
        p[0] |= 0xC0; // LI 3: clock unsynchronized
        assert_eq!(parse_reply(&p), None);
        let mut p = reply_with(3_976_214_400);
        p[1] = 0; // kiss-of-death
        assert_eq!(parse_reply(&p), None);
        let mut p = reply_with(3_976_214_400);
        p[1] = 16; // reserved stratum
        assert_eq!(parse_reply(&p), None);
        assert_eq!(parse_reply(&reply_with(0)), None); // zero timestamp
        assert_eq!(parse_reply(&reply_with(1)), None); // pre-Unix-epoch
    }
}
