//! Fixture packages built with a nested `cargo`, for the tests that need a
//! real link: where the section lands and whether rebuilds keep it fresh.

#![allow(dead_code)]

use std::ffi::OsStr;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::{env, fs};

use object::{BinaryFormat, Object, ObjectSection, ObjectSymbol};

/// A package (or workspace) in a temporary directory, built into its own
/// target directory.
pub struct Fixture {
    _tmp: tempfile::TempDir,
    pub dir: PathBuf,
    pub target: PathBuf,
}

impl Fixture {
    pub fn new(name: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        // Shared across runs, so dependencies stay compiled.
        let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
        // The workspace's lock, so the fixture builds the same versions.
        let lock = crate_dir().join("../Cargo.lock");
        fs::copy(lock, dir.join("Cargo.lock")).unwrap();
        Self {
            _tmp: tmp,
            dir,
            target,
        }
    }

    pub fn file(&self, path: &str, contents: &str) -> &Self {
        let path = self.dir.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
        self
    }

    /// `cargo <args>` in the fixture, without any build-info override from
    /// the environment the tests run in.
    pub fn cargo<I, S>(&self, args: I) -> Output
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.cargo_env(args, &[])
    }

    /// [`cargo`](Self::cargo) with `vars` set.
    pub fn cargo_env<I, S>(&self, args: I, vars: &[(&str, &str)]) -> Output
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let out = Command::new(env::var_os("CARGO").unwrap_or("cargo".into()))
            .args(args)
            .arg("--target-dir")
            .arg(&self.target)
            .current_dir(&self.dir)
            .env_remove("AIMDB_BUILD_REVISION")
            .env_remove("AIMDB_BUILD_DIRTY")
            .env_remove("AIMDB_BUILD_INFO")
            .env_remove("CARGO_TARGET_DIR")
            .envs(vars.iter().copied())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "cargo failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    /// `git <args>` in the fixture, as a fixed committer; its trimmed stdout.
    pub fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(&self.dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap().trim().into()
    }

    pub fn bin(&self, profile: &str, name: &str) -> PathBuf {
        self.target
            .join(profile)
            .join(format!("{name}{}", env::consts::EXE_SUFFIX))
    }

    /// The newest section image a build script of package `package` wrote.
    pub fn image(&self, profile: &str, package: &str) -> Vec<u8> {
        let build = self.target.join(profile).join("build");
        let newest = fs::read_dir(&build)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|n| n.starts_with(&format!("{package}-")))
            })
            .map(|p| p.join("out").join(aimdb_build_info::FILE_NAME))
            .filter(|p| p.exists())
            .max_by_key(|p| fs::metadata(p).unwrap().modified().unwrap())
            .unwrap_or_else(|| panic!("no image under {}", build.display()));
        fs::read(newest).unwrap()
    }
}

/// This crate's directory.
pub fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The `[dependencies]` and `[build-dependencies]` lines for this crate.
pub fn deps() -> String {
    let path = crate_dir().display().to_string().replace('\\', "/");
    format!(
        "[dependencies]\naimdb-build-info = {{ path = \"{path}\" }}\n\n\
         [build-dependencies]\naimdb-build-info = {{ path = \"{path}\", features = [\"build\"] }}\n"
    )
}

/// A `main` that prints the section's post-link state and payload.
pub const REPORTING_MAIN: &str = r#"
use aimdb_build_info::{parse, PostLink};

fn main() {
    let section = parse(aimdb_build_info::section!()).expect("build info");
    match section.post_link() {
        PostLink::None => println!("none"),
        PostLink::Erased => println!("erased"),
        PostLink::Written(data) => {
            let hex: String = data.iter().map(|b| format!("{b:02x}")).collect();
            println!("written {hex}");
        }
        PostLink::Malformed => println!("malformed"),
    }
    println!("{}", String::from_utf8_lossy(section.payload));
}
"#;

/// Run `bin` and return its stdout.
pub fn run(bin: &Path) -> String {
    let out = Command::new(bin).output().unwrap();
    assert!(out.status.success(), "{}: {out:?}", bin.display());
    String::from_utf8(out.stdout).unwrap()
}

/// Where the section lies in a linked binary, found by section name. Also
/// checks that the global symbol is at the section's start.
pub fn locate(binary: &[u8]) -> Range<usize> {
    let file = object::File::parse(binary).unwrap();
    let section = match file.format() {
        BinaryFormat::Elf => file.section_by_name(aimdb_build_info::ELF_SECTION),
        BinaryFormat::MachO => {
            let (segment, name) = aimdb_build_info::MACHO_SECTION;
            file.sections().find(|s| {
                s.segment_name().ok().flatten() == Some(segment) && s.name().ok() == Some(name)
            })
        }
        other => panic!("no build-info section on {other:?}"),
    }
    .expect("section by name");

    let symbol = file
        .symbols()
        .find(|s| {
            s.name().is_ok_and(|n| {
                n.trim_start_matches('_') == aimdb_build_info::SYMBOL.trim_start_matches('_')
            })
        })
        .expect("section by symbol");
    assert_eq!(
        symbol.address(),
        section.address(),
        "symbol at section start"
    );

    let (offset, size) = section.file_range().expect("section has file bytes");
    offset as usize..(offset + size) as usize
}

/// Re-sign after patching: macOS refuses to run an arm64 binary whose
/// signature no longer matches.
pub fn resign(bin: &Path) {
    if cfg!(target_vendor = "apple") {
        let status = Command::new("codesign")
            .args(["-f", "-s", "-"])
            .arg(bin)
            .status()
            .unwrap();
        assert!(status.success());
    }
}
