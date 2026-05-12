use std::fs;
use std::path::Path;

#[test]
fn readme_exists() {
    let readme_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
    assert!(readme_path.exists(), "README.md should exist");
}

#[test]
fn readme_contains_title() {
    let content = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
        .expect("failed to read README.md");
    assert!(content.contains("# hello-spmc-ring-async"));
}

#[test]
fn readme_contains_run_command() {
    let content = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
        .expect("failed to read README.md");
    assert!(content.contains("cargo run -p hello-spmc-ring-async"));
}

#[test]
fn readme_explains_spmc_ring_semantics() {
    let content = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
        .expect("failed to read README.md");
    assert!(content.contains("bounded ring buffer"));
    assert!(content.contains("read cursor"));
    assert!(content.contains("oldest entries are overwritten"));
}

#[test]
fn readme_contrasts_with_alternatives() {
    let content = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
        .expect("failed to read README.md");
    assert!(content.contains("SingleLatest"));
    assert!(content.contains("Mailbox"));
}
