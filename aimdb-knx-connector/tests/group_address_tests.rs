//! Unit tests for KNX group address parsing and formatting

#[cfg(feature = "tokio-runtime")]
mod tokio_tests {
    use aimdb_knx_connector::GroupAddress;

    #[test]
    fn test_group_address_parsing_valid() {
        // Valid 3-level format
        assert_eq!(parse_address("0/0/0"), Ok(0x0000));
        assert_eq!(parse_address("1/0/7"), Ok(0x0807));
        assert_eq!(parse_address("5/3/128"), Ok(0x2B80));
        assert_eq!(parse_address("31/7/255"), Ok(0xFFFF));
    }

    #[test]
    fn test_group_address_parsing_invalid() {
        // Out of range
        assert!(parse_address("32/0/0").is_err()); // Main > 31
        assert!(parse_address("0/8/0").is_err()); // Middle > 7
        assert!(parse_address("1/0/256").is_err()); // Sub > 255

        // Invalid format
        assert!(parse_address("1/0").is_err()); // Missing sub
        assert!(parse_address("1").is_err()); // Missing middle and sub
        assert!(parse_address("abc/def/ghi").is_err()); // Non-numeric
        assert!(parse_address("").is_err()); // Empty
    }

    #[test]
    fn test_group_address_formatting() {
        assert_eq!(format_address(0x0000), "0/0/0");
        assert_eq!(format_address(0x0807), "1/0/7");
        assert_eq!(format_address(0x2B80), "5/3/128");
        assert_eq!(format_address(0xFFFF), "31/7/255");
    }

    #[test]
    fn test_group_address_roundtrip() {
        let addresses = vec![
            "0/0/0", "1/0/7", "5/3/128", "31/7/255", "10/2/64", "20/5/200",
        ];

        for addr in addresses {
            let raw = parse_address(addr).unwrap();
            let formatted = format_address(raw);
            assert_eq!(formatted, addr, "Roundtrip failed for {}", addr);
        }
    }

    #[test]
    fn test_group_address_from_knx_pico() {
        // Test GroupAddress from knx-pico crate
        let addr = GroupAddress::from(0x0807); // 1/0/7
        assert_eq!(addr.main(), 1);
        assert_eq!(addr.middle(), 0);
        assert_eq!(addr.sub(), 7);

        let addr = GroupAddress::from(0xFFFF); // 31/7/255
        assert_eq!(addr.main(), 31);
        assert_eq!(addr.middle(), 7);
        assert_eq!(addr.sub(), 255);
    }

    // Helper functions (would be in the actual connector code)
    fn parse_address(addr_str: &str) -> Result<u16, String> {
        let parts: Vec<&str> = addr_str.split('/').collect();

        if parts.len() != 3 {
            return Err(format!("Invalid format: {}", addr_str));
        }

        let main: u8 = parts[0]
            .parse()
            .map_err(|_| format!("Invalid main: {}", parts[0]))?;
        let middle: u8 = parts[1]
            .parse()
            .map_err(|_| format!("Invalid middle: {}", parts[1]))?;
        let sub: u8 = parts[2]
            .parse()
            .map_err(|_| format!("Invalid sub: {}", parts[2]))?;

        if main > 31 {
            return Err(format!("Main must be 0-31, got {}", main));
        }
        if middle > 7 {
            return Err(format!("Middle must be 0-7, got {}", middle));
        }

        Ok(((main as u16) << 11) | ((middle as u16) << 8) | (sub as u16))
    }

    fn format_address(raw: u16) -> String {
        let main = (raw >> 11) & 0x1F;
        let middle = (raw >> 8) & 0x07;
        let sub = raw & 0xFF;
        format!("{}/{}/{}", main, middle, sub)
    }
}
