//! Every crate's `events.rs` logs with explicit `tracing`/`log` targets naming
//! the module that raises each event (`mod target`), so filters such as
//! `aimdb_core::session::server=warn` keep matching. A literal does not follow
//! a module rename; this test fails when a target no longer names a real file.

use std::{
    fs,
    path::{Path, PathBuf},
};

fn events_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            if name != "target"
                && name != "node_modules"
                && !name.to_string_lossy().starts_with('.')
            {
                events_files(&path, out);
            }
        } else if name == "events.rs" && path.components().any(|c| c.as_os_str() == "src") {
            out.push(path);
        }
    }
}

/// The `"crate::module::path"` literals inside `mod target { ... }`.
fn targets(source: &str) -> Vec<String> {
    let Some(start) = source.find("mod target {") else {
        return Vec::new();
    };
    let block = &source[start..start + source[start..].find('}').unwrap()];
    block
        .lines()
        .filter(|line| line.contains("const "))
        .map(|line| line.split('"').nth(1).unwrap().to_owned())
        .collect()
}

/// The crate directory whose package name is `crate_name` (`-` for `_`).
fn crate_dir(root: &Path, crate_name: &str) -> Option<PathBuf> {
    [root.to_path_buf(), root.join("tools")]
        .iter()
        .map(|base| base.join(crate_name.replace('_', "-")))
        .find(|dir| dir.join("Cargo.toml").is_file())
}

fn module_exists(root: &Path, target: &str) -> bool {
    let mut segments = target.split("::");
    let Some(dir) = crate_dir(root, segments.next().unwrap()) else {
        return false;
    };
    let rest: Vec<_> = segments.collect();
    let src = dir.join("src");
    if rest.is_empty() {
        return src.join("lib.rs").is_file();
    }
    let module = src.join(rest.join("/"));
    module.with_extension("rs").is_file() || module.join("mod.rs").is_file()
}

#[test]
fn log_targets_name_real_modules() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut files = Vec::new();
    events_files(root, &mut files);

    let mut checked = 0;
    let mut missing = Vec::new();
    for file in &files {
        for target in targets(&fs::read_to_string(file).unwrap()) {
            checked += 1;
            if !module_exists(root, &target) {
                missing.push(format!("{}: {target}", file.display()));
            }
        }
    }

    assert!(
        checked > 0,
        "no `mod target` found under {}",
        root.display()
    );
    assert!(
        missing.is_empty(),
        "log targets naming no module:\n{}",
        missing.join("\n")
    );
}
