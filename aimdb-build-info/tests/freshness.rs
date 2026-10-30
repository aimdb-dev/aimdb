//! The cargo behavior the build-script helper's rerun tracking relies on, in
//! real builds: no spurious rerun, a directory watch that sees new and removed
//! files, and the documented limit. Which paths get watched, the overrides and
//! `track_dirty` are unit-tested in `src/build/tests.rs`. Run with
//! `cargo test -p aimdb-build-info -- --ignored`.

mod common;

use std::fs::{self, File};
use std::time::{Duration, SystemTime};

use common::{locate, run, Fixture, REPORTING_MAIN};

/// A workspace in a git repository: `app` embeds the section, `gen` turns the
/// files in `gen/assets/` into code, as asset-embedding crates do.
fn fixture() -> Fixture {
    let fx = Fixture::new("freshness");
    fx.file(
        "Cargo.toml",
        "[workspace]\nmembers = [\"app\", \"gen\"]\nresolver = \"2\"\n",
    )
    .file(".gitignore", "*.tmp\n")
    .file(
        "gen/Cargo.toml",
        "[package]\nname = \"gen\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .file(
        "gen/build.rs",
        r#"use std::{env, fs, path::Path};

fn main() {
    println!("cargo:rerun-if-changed=assets");
    let mut names: Vec<String> = fs::read_dir("assets")
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    let out = Path::new(&env::var("OUT_DIR").unwrap()).join("assets.rs");
    fs::write(out, format!("pub const ASSETS: &[&str] = &{names:?};\n")).unwrap();
}
"#,
    )
    .file(
        "gen/src/lib.rs",
        "include!(concat!(env!(\"OUT_DIR\"), \"/assets.rs\"));\n",
    )
    .file("gen/assets/a.txt", "a")
    .file(
        "app/Cargo.toml",
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n{}",
            common::deps().replace(
                "[dependencies]\n",
                "[dependencies]\ngen = { path = \"../gen\" }\n"
            )
        ),
    )
    .file(
        "app/build.rs",
        r#"fn main() {
    println!("cargo:rerun-if-env-changed=FX_WATCH");
    let mut info = aimdb_build_info::BuildInfo::from_cargo_env();
    if std::env::var("FX_WATCH").as_deref() == Ok("1") {
        info = info.watch("../gen/assets");
    }
    info.write().expect("build info");
}
"#,
    )
    .file(
        "app/src/main.rs",
        &format!("{REPORTING_MAIN}\n#[allow(dead_code)]\nfn assets() -> usize {{\n    gen::ASSETS.len()\n}}\n"),
    );
    fx
}

struct Build {
    /// Whether `app`'s build script ran.
    reran: bool,
    /// The payload the binary reports.
    payload: String,
}

/// Build `app` and check that the binary's section is the image its build
/// script wrote.
fn build(fx: &Fixture, vars: &[(&str, &str)]) -> Build {
    let out = fx.cargo_env(["build", "-v", "-p", "app"], vars);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let reran = stderr
        .lines()
        .any(|l| l.contains("Running") && l.contains("app-") && l.contains("build-script-build`"));

    let bin = fx.bin("debug", "app");
    let bytes = fs::read(&bin).unwrap();
    let start = locate(&bytes).start;
    let image = fx.image("debug", "app");
    assert_eq!(
        &bytes[start..start + image.len()],
        &image[..],
        "section is the image"
    );

    let stdout = run(&bin);
    let payload = stdout.lines().nth(1).unwrap().to_owned();
    Build { reran, payload }
}

#[track_caller]
fn assert_dirty(b: &Build, dirty: &str) {
    let field = format!(r#""dirty":{dirty}"#);
    assert!(b.payload.contains(&field), "want {field}: {}", b.payload);
}

#[test]
#[ignore = "builds a fixture workspace"]
fn section_follows_the_repository() {
    let fx = fixture();
    fx.git(&["init", "-q"]);
    // The first build settles Cargo.lock; then everything is committed.
    build(&fx, &[]);
    fx.git(&["add", "-A"]);
    fx.git(&["commit", "-q", "-m", "init"]);

    // Commit: HEAD moved, clean.
    let b = build(&fx, &[]);
    assert!(b.reran);
    assert_dirty(&b, "false");
    let head = fx.git(&["rev-parse", "HEAD"]);
    assert!(b.payload.contains(&format!(r#""revision":"{head}""#)));

    // Nothing changed: no rerun (`git status` does not touch a watched file).
    let b = build(&fx, &[]);
    assert!(!b.reran, "spurious rerun");
    assert_dirty(&b, "false");

    // A new asset, even with an mtime older than the last build, is seen by
    // the directory watch; so is its removal.
    fx.file("gen/assets/b.txt", "b");
    let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    File::options()
        .write(true)
        .open(fx.dir.join("gen/assets/b.txt"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    assert_dirty(&build(&fx, &[]), "true");
    fs::remove_file(fx.dir.join("gen/assets/b.txt")).unwrap();
    assert_dirty(&build(&fx, &[]), "false");

    // The documented limit: an ignored file in `assets/` means it is no
    // longer watched whole, so a new untracked file there goes unnoticed.
    fx.file("gen/assets/scratch.tmp", "ignored");
    assert_dirty(&build(&fx, &[]), "false");
    fx.file("gen/assets/c.txt", "c");
    let b = build(&fx, &[]);
    assert!(!b.reran, "the limit is gone; update the docs");
    assert_dirty(&b, "false");
    // `watch()` covers it.
    assert_dirty(&build(&fx, &[("FX_WATCH", "1")]), "true");
}
