//! Print a binary's build-info section, and optionally check where it lies.
//!
//! ```text
//! cargo run -p aimdb-build-info --example inspect -- <binary> \
//!     [--section-addr 0x08000400] [--text-addr 0x08002000]
//! ```
//!
//! Exits non-zero when the section is missing or malformed, or an address
//! differs from the one given.

use std::process::ExitCode;

use aimdb_build_info::{parse, PostLink, ELF_SECTION, MACHO_SECTION};
use object::{BinaryFormat, Object, ObjectSection};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("inspect: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: inspect <binary> [--section-addr ADDR] [--text-addr ADDR]")?;
    let (mut section_addr, mut text_addr) = (None, None);
    while let Some(flag) = args.next() {
        let value = args.next().ok_or(format!("{flag} needs a value"))?;
        let addr = parse_addr(&value)?;
        match flag.as_str() {
            "--section-addr" => section_addr = Some(addr),
            "--text-addr" => text_addr = Some(addr),
            _ => return Err(format!("unknown flag {flag}")),
        }
    }

    let bytes = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
    let file = object::File::parse(&*bytes).map_err(|e| format!("{path}: {e}"))?;
    let section = match file.format() {
        BinaryFormat::Elf => file.section_by_name(ELF_SECTION),
        BinaryFormat::MachO => file.sections().find(|s| {
            s.segment_name().ok().flatten() == Some(MACHO_SECTION.0)
                && s.name().ok() == Some(MACHO_SECTION.1)
        }),
        other => return Err(format!("no build-info section on {other:?}")),
    }
    .ok_or("no build-info section")?;
    let (offset, size) = section
        .file_range()
        .ok_or("the section has no file bytes")?;
    let data = &bytes[offset as usize..(offset + size) as usize];
    let parsed = parse(data).map_err(|e| e.to_string())?;

    println!(
        "section:   {:#010x}, {size} bytes at file offset {offset:#x}",
        section.address()
    );
    println!("format:    {}", parsed.format);
    println!("payload:   {}", String::from_utf8_lossy(parsed.payload));
    let post_link = match parsed.post_link() {
        PostLink::None => "none".to_string(),
        PostLink::Erased => format!("erased, {} bytes", parsed.region.len()),
        PostLink::Written(d) => format!("written, {} of {} bytes", d.len(), parsed.region.len()),
        PostLink::Malformed => "malformed".to_string(),
    };
    println!("post-link: {post_link}");
    let text = file.section_by_name(".text").map(|s| s.address());
    if let Some(text) = text {
        println!(".text:     {text:#010x}");
    }

    if let Some(want) = section_addr {
        if section.address() != want {
            return Err(format!(
                "section at {:#x}, expected {want:#x}",
                section.address()
            ));
        }
    }
    if let Some(want) = text_addr {
        if text != Some(want) {
            return Err(format!("`.text` at {text:#x?}, expected {want:#x}"));
        }
    }
    Ok(())
}

fn parse_addr(s: &str) -> Result<u64, String> {
    let digits = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"));
    match digits {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => s.parse(),
    }
    .map_err(|e| format!("bad address {s}: {e}"))
}
