use super::*;

fn scan(source: &str) -> Vec<String> {
    let file = syn::parse_file(source).unwrap();
    let mut scanner = Scanner {
        file: "fixture.rs".into(),
        module_dir: PathBuf::new(),
        path_dir: PathBuf::new(),
        failures: vec![],
    };
    if !excluded(&file.attrs) {
        scanner.visit_file(&file);
    }
    scanner.failures
}

#[test]
fn production_installations_are_rejected() {
    for source in [
        "fn install() { log::set_logger(&LOGGER); }",
        "#[cfg(feature = \"log\")] fn install() { log::set_max_level(level); }",
        "#[cfg(not(test))] mod production { fn install() { std::panic::set_hook(hook); } }",
        "#[cfg(any(test, feature = \"log\"))] fn install() { log::set_logger(&LOGGER); }",
        "fn install() { tracing_subscriber::fmt().init(); }",
        "fn install() { unsafe { libc::pthread_atfork(None, None, None); } }",
        "macro_rules! install { () => { log::set_logger(&LOGGER); }; }",
        "fn install() { wrapper!(log::set_logger(&LOGGER)); }",
        "use log::set_logger as install;",
        "fn f() { logger.set_max_level(level); }",
    ] {
        assert!(!scan(source).is_empty(), "missed production code: {source}");
    }
}

#[test]
fn only_explicitly_test_only_code_is_excluded() {
    for source in [
        "#[cfg(test)] mod tests { fn install() { log::set_logger(&LOGGER); } }",
        "#[cfg(all(test, feature = \"log\"))] mod tests { fn install() { log::set_logger(&LOGGER); } }",
        "#[cfg(not(not(test)))] fn install() { log::set_logger(&LOGGER); }",
        "mod tests { #![cfg(test)] fn f() { log::set_logger(&LOGGER); } }",
        "fn f() { #[cfg(test)] let _ = log::set_logger(&LOGGER); }",
        "#![cfg(test)] fn install() { log::set_logger(&LOGGER); }",
        "struct X; impl X { #[cfg(test)] fn install() { log::set_logger(&LOGGER); } }",
        "fn f() { #[cfg(test)] { log::set_logger(&LOGGER); } }",
        "fn f() { let text = r#\"log::set_logger(&LOGGER)\"#; /* log::set_logger(&LOGGER); */ }",
    ] {
        assert!(scan(source).is_empty(), "flagged test or literal: {source}");
    }
    // A test module must not suppress production siblings or its enclosing module.
    let failures = scan("mod parent { #[cfg(test)] mod tests { fn f() { log::set_logger(&LOGGER); } } fn production() { log::set_logger(&LOGGER); } }");
    assert_eq!(failures.len(), 1);
}

#[test]
fn external_modules_inherit_test_exclusion_and_missing_files_fail_closed() {
    let root = env::temp_dir().join(format!("aimdb-global-guard-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("lib.rs"),
        "#[cfg(test)] mod tests; mod production;",
    )
    .unwrap();
    fs::write(
        root.join("tests.rs"),
        "fn f() { log::set_logger(&LOGGER); }",
    )
    .unwrap();
    fs::write(
        root.join("production.rs"),
        "fn f() { log::set_logger(&LOGGER); }",
    )
    .unwrap();
    let mut scanner = Scanner {
        file: PathBuf::new(),
        module_dir: PathBuf::new(),
        path_dir: PathBuf::new(),
        failures: vec![],
    };
    scanner.scan_file(&root.join("lib.rs"), root.clone());
    assert_eq!(scanner.failures.len(), 1);
    assert!(scanner.failures[0].contains("production.rs:1:"));
    // Explicit paths in an external module are relative to its source file.
    fs::write(
        root.join("production.rs"),
        "#[path = \"tests.rs\"] mod renamed;",
    )
    .unwrap();
    scanner.failures.clear();
    scanner.scan_file(&root.join("lib.rs"), root.clone());
    assert_eq!(scanner.failures.len(), 1);
    assert!(scanner.failures[0].contains("tests.rs:1:"));
    fs::write(root.join("lib.rs"), "mod missing;").unwrap();
    scanner.failures.clear();
    scanner.scan_file(&root.join("lib.rs"), root.clone());
    assert_eq!(scanner.failures.len(), 1);
    fs::write(root.join("lib.rs"), "this is invalid Rust").unwrap();
    scanner.failures.clear();
    scanner.scan_file(&root.join("lib.rs"), root.clone());
    assert_eq!(scanner.failures.len(), 1);
    fs::remove_dir_all(root).unwrap();
}
