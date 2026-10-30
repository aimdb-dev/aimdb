//! The section in a real host binary: release with fat LTO and no AimDB,
//! found by name and by symbol, and patched after linking without changing
//! the file size. Run with `cargo test -p aimdb-build-info -- --ignored`.

mod common;

use std::fs;

use common::{locate, resign, run, Fixture, REPORTING_MAIN};

fn fixture() -> Fixture {
    let fx = Fixture::new("host-link");
    fx.file(
        "Cargo.toml",
        &format!(
            "[package]\nname = \"bi-host\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [workspace]\n\n[profile.release]\nlto = \"fat\"\ncodegen-units = 1\n\n{}",
            common::deps()
        ),
    )
    .file(
        "build.rs",
        "fn main() {\n    aimdb_build_info::BuildInfo::from_cargo_env()\n        \
         .kind(\"test\")\n        .post_link_len(64)\n        .write()\n        \
         .expect(\"build info\");\n}\n",
    )
    .file("src/main.rs", REPORTING_MAIN)
    // Nothing calls `unused`: only the section's retain flag keeps it.
    .file(
        "src/bin/bare.rs",
        "#[allow(dead_code)]\nfn unused() -> &'static [u8] {\n    \
         aimdb_build_info::section!()\n}\n\nfn main() {}\n",
    );
    fx
}

#[test]
#[ignore = "builds a fixture package"]
fn release_lto_binary_carries_a_patchable_section() {
    let fx = fixture();
    fx.cargo(["build", "--release", "--bins"]);
    let image = fx.image("release", "bi-host");
    let section = aimdb_build_info::parse(&image).unwrap();
    assert!(String::from_utf8_lossy(section.payload).contains(r#""kind":"test""#));

    for bin in ["bi-host", "bare"] {
        let bytes = fs::read(fx.bin("release", bin)).unwrap();
        let range = locate(&bytes);
        assert!(range.len() >= image.len(), "{bin}");
        assert_eq!(
            &bytes[range.start..range.start + image.len()],
            &image[..],
            "{bin}"
        );
    }

    let bin = fx.bin("release", "bi-host");
    assert!(run(&bin).starts_with("erased\n"));

    // Write the region in the file: a length, then the bytes.
    let mut bytes = fs::read(&bin).unwrap();
    let len = bytes.len();
    let start = locate(&bytes).start;
    let region = start + u32::from_le_bytes(image[16..20].try_into().unwrap()) as usize;
    bytes[region..region + 4].copy_from_slice(&3u32.to_le_bytes());
    bytes[region + 4..region + 7].copy_from_slice(b"abc");
    fs::write(&bin, &bytes).unwrap();
    assert_eq!(
        fs::metadata(&bin).unwrap().len() as usize,
        len,
        "size unchanged"
    );
    resign(&bin);

    let patched = fs::read(&bin).unwrap();
    let range = locate(&patched);
    let section = aimdb_build_info::parse(&patched[range]).unwrap();
    assert_eq!(
        section.post_link(),
        aimdb_build_info::PostLink::Written(b"abc")
    );
    // The running program reads the patched bytes, not a folded constant.
    assert!(run(&bin).starts_with("written 616263\n"));
}
