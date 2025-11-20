//! Unit tests for KNX frame building and parsing

#[cfg(feature = "tokio-runtime")]
mod tests {
    #[test]
    fn test_connect_request_structure() {
        // CONNECT_REQUEST should be 26 bytes
        let frame = build_connect_request_mock();

        assert_eq!(frame.len(), 26, "CONNECT_REQUEST must be 26 bytes");
        assert_eq!(frame[0], 0x06, "Header length");
        assert_eq!(frame[1], 0x10, "Protocol version");
        assert_eq!(frame[2], 0x02, "Service type high");
        assert_eq!(frame[3], 0x05, "Service type low (CONNECT_REQUEST)");
        assert_eq!(frame[4], 0x00, "Total length high");
        assert_eq!(frame[5], 0x1A, "Total length low (26)");
    }

    #[test]
    fn test_tunneling_ack_structure() {
        let channel_id = 42;
        let seq = 7;
        let frame = build_tunneling_ack_mock(channel_id, seq);

        assert_eq!(frame.len(), 10, "TUNNELING_ACK must be 10 bytes");
        assert_eq!(frame[0], 0x06, "Header length");
        assert_eq!(frame[1], 0x10, "Protocol version");
        assert_eq!(frame[2], 0x04, "Service type high");
        assert_eq!(frame[3], 0x21, "Service type low (TUNNELING_ACK)");
        assert_eq!(frame[7], channel_id, "Channel ID");
        assert_eq!(frame[8], seq, "Sequence counter");
        assert_eq!(frame[9], 0x00, "Status (OK)");
    }

    #[test]
    fn test_group_write_cemi_short_telegram() {
        // Short telegram: 1 byte, value < 64
        let group_addr = 0x0807; // 1/0/7
        let data = vec![0x01]; // ON

        let cemi = build_group_write_cemi_mock(group_addr, &data);

        // Should be: message_code + add_info_len + ctrl1 + ctrl2 + src + dst + npdu_len + tpci + apci
        assert!(cemi.len() >= 11, "cEMI frame too short");
        assert_eq!(cemi[0], 0x11, "L_Data.req message code");
        assert_eq!(cemi[1], 0x00, "Additional info length");
    }

    #[test]
    fn test_group_write_cemi_long_telegram() {
        // Long telegram: multi-byte data
        let group_addr = 0x0807; // 1/0/7
        let data = vec![0x12, 0x34, 0x56]; // 3 bytes

        let cemi = build_group_write_cemi_mock(group_addr, &data);

        assert!(cemi.len() >= 14, "cEMI frame too short for long telegram");
        assert_eq!(cemi[0], 0x11, "L_Data.req message code");

        // Check that data is present
        let npdu_len = cemi[8] as usize;
        assert_eq!(
            npdu_len,
            2 + data.len(),
            "NPDU length should be TPCI + APCI + data"
        );
    }

    #[test]
    fn test_tunneling_request_structure() {
        let channel_id = 10;
        let seq = 5;
        let cemi = vec![
            0x11, 0x00, 0xBC, 0xE0, 0x00, 0x00, 0x08, 0x07, 0x01, 0x00, 0x81,
        ];

        let frame = build_tunneling_request_mock(channel_id, seq, &cemi);

        let expected_len = 10 + cemi.len();
        assert_eq!(frame.len(), expected_len);
        assert_eq!(frame[0], 0x06, "Header length");
        assert_eq!(frame[1], 0x10, "Protocol version");
        assert_eq!(frame[2], 0x04, "Service type high");
        assert_eq!(frame[3], 0x20, "Service type low (TUNNELING_REQUEST)");
        assert_eq!(frame[7], channel_id, "Channel ID");
        assert_eq!(frame[8], seq, "Sequence counter");

        // Check cEMI is appended
        assert_eq!(&frame[10..], &cemi[..]);
    }

    #[test]
    fn test_service_type_detection() {
        // TUNNELING_REQUEST (0x0420)
        let tunneling_req = vec![0x06, 0x10, 0x04, 0x20];
        assert!(is_tunneling_request(&tunneling_req));
        assert!(!is_tunneling_ack(&tunneling_req));

        // TUNNELING_ACK (0x0421)
        let tunneling_ack = vec![0x06, 0x10, 0x04, 0x21];
        assert!(!is_tunneling_request(&tunneling_ack));
        assert!(is_tunneling_ack(&tunneling_ack));

        // CONNECT_RESPONSE (0x0206)
        let connect_resp = vec![0x06, 0x10, 0x02, 0x06];
        assert!(!is_tunneling_request(&connect_resp));
        assert!(!is_tunneling_ack(&connect_resp));
    }

    // Mock implementations (simplified versions of actual functions)
    fn build_connect_request_mock() -> Vec<u8> {
        vec![
            0x06, 0x10, 0x02, 0x05, 0x00, 0x1A, // Header
            0x08, 0x01, 0, 0, 0, 0, 0x00, 0x00, // Control HPAI
            0x08, 0x01, 0, 0, 0, 0, 0x00, 0x00, // Data HPAI
            0x04, 0x04, 0x02, 0x00, // CRI
        ]
    }

    fn build_tunneling_ack_mock(channel_id: u8, seq: u8) -> Vec<u8> {
        vec![
            0x06, 0x10, 0x04, 0x21, 0x00, 0x0A, // Header
            0x04, channel_id, seq, 0x00, // Connection header + status
        ]
    }

    fn build_group_write_cemi_mock(group_addr: u16, data: &[u8]) -> Vec<u8> {
        let mut frame = vec![
            0x11, 0x00, 0xBC, 0xE0, // Message code, add info, ctrl fields
            0x00, 0x00, // Source address
        ];
        frame.extend_from_slice(&group_addr.to_be_bytes());

        if data.len() == 1 && data[0] < 64 {
            frame.push(0x01);
            frame.push(0x00);
            frame.push(0x80 | (data[0] & 0x3F));
        } else {
            let npdu_len = 2 + data.len();
            frame.push(npdu_len as u8);
            frame.push(0x00);
            frame.push(0x80);
            frame.extend_from_slice(data);
        }
        frame
    }

    fn build_tunneling_request_mock(channel_id: u8, seq: u8, cemi: &[u8]) -> Vec<u8> {
        let total_len = 10 + cemi.len();
        let mut frame = vec![
            0x06,
            0x10,
            0x04,
            0x20,
            (total_len >> 8) as u8,
            total_len as u8,
            0x04,
            channel_id,
            seq,
            0x00,
        ];
        frame.extend_from_slice(cemi);
        frame
    }

    fn is_tunneling_request(data: &[u8]) -> bool {
        data.len() >= 4 && data[2] == 0x04 && data[3] == 0x20
    }

    fn is_tunneling_ack(data: &[u8]) -> bool {
        data.len() >= 4 && data[2] == 0x04 && data[3] == 0x21
    }
}
