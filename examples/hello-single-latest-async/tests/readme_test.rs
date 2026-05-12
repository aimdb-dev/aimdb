//! Tests verifying the README example claims about SingleLatest semantics.

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn readme_exists() {
        let readme_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
        assert!(
            readme_path.exists(),
            "README.md should exist in the example crate root"
        );
    }

    #[test]
    fn readme_contains_required_sections() {
        let readme_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
        let content = std::fs::read_to_string(readme_path).expect("Failed to read README.md");

        assert!(
            content.contains("# Hello SingleLatest Async"),
            "README should have a title"
        );
        assert!(
            content.contains("## What is SingleLatest?"),
            "README should explain SingleLatest semantics"
        );
        assert!(
            content.contains("## Running the Example"),
            "README should explain how to run the example"
        );
        assert!(
            content.contains("cargo run -p hello-single-latest-async"),
            "README should contain the run command"
        );
    }

    #[test]
    fn readme_is_valid_utf8_and_nonempty() {
        let readme_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
        let content = std::fs::read_to_string(readme_path).expect("Failed to read README.md");

        assert!(!content.is_empty(), "README should not be empty");
        assert!(
            content.len() > 100,
            "README should have meaningful content"
        );
    }
}
