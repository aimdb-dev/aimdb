use std::fs;
use std::path::Path;

#[test]
fn readme_exists() {
    let readme_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
    assert!(readme_path.exists(), "README.md should exist");
}

#[test]
fn readme_contains_required_sections() {
    let readme_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
    let content = fs::read_to_string(readme_path).expect("Failed to read README.md");

    assert!(content.contains("# Hello Mailbox (Async)"), "README should have a title");
    assert!(
        content.contains("How it differs from the sync sibling"),
        "README should explain the difference from the sync example"
    );
    assert!(
        content.contains("Running"),
        "README should include a Running section"
    );
}

#[test]
fn readme_is_not_empty() {
    let readme_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
    let content = fs::read_to_string(readme_path).expect("Failed to read README.md");
    assert!(
        content.len() > 100,
        "README should contain meaningful content"
    );
}
