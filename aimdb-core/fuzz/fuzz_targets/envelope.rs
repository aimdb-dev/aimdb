#![no_main]
use aimdb_core::envelope::{Envelope, HeaderWriter};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(env) = Envelope::parse(data) {
        // A successful parse borrows a suffix of the supplied frame.
        let start = data.len() - env.payload().len();
        assert_eq!(env.payload().as_ptr(), data[start..].as_ptr());
        // Reserved fields may be skipped, but the logical identity survives
        // canonical re-encoding in caller-owned storage.
        let mut bytes = [0; 19];
        let n = HeaderWriter::new(&mut bytes)
            .contract(env.contract())
            .codec(env.codec())
            .finish()
            .unwrap();
        let canonical = Envelope::parse(&bytes[..n]).unwrap();
        assert_eq!(canonical.contract(), env.contract());
        assert_eq!(canonical.codec(), env.codec());
        assert!(canonical.payload().is_empty());
    }
});
