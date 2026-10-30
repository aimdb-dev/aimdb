use std::collections::BTreeMap;
use std::string::String;
use std::vec::Vec;
use std::{fs, vec};

use serde_json::{json, Value};

use super::*;

/// A package directory with Cargo's build-script variables pointing at it.
struct Package {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    vars: BTreeMap<String, String>,
}

impl Package {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        // Canonical: git reports /private/var/… for macOS's /var/….
        let dir = tmp.path().canonicalize().unwrap();
        let mut vars = BTreeMap::new();
        for (k, v) in [
            ("CARGO_MANIFEST_DIR", dir.to_str().unwrap()),
            ("CARGO_PKG_NAME", "app"),
            ("CARGO_PKG_VERSION", "1.2.3"),
            ("PROFILE", "release"),
            ("TARGET", "thumbv8m.main-none-eabihf"),
        ] {
            vars.insert(k.into(), v.into());
        }
        Self {
            _tmp: tmp,
            dir,
            vars,
        }
    }

    fn set(&mut self, k: &str, v: &str) -> &mut Self {
        self.vars.insert(k.into(), v.into());
        self
    }

    fn file(&self, path: &str, contents: &str) -> &Self {
        let path = self.dir.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
        self
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(&self.dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap().trim().into()
    }

    fn repo(&self) -> &Self {
        self.git(&["init", "-q"]);
        self
    }

    fn commit(&self) -> &Self {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", "c"]);
        self
    }

    fn compose(&self, info: &BuildInfo) -> Result<Composed, Error> {
        info.compose(&|k| self.vars.get(k).cloned())
    }

    fn payload(&self, info: &BuildInfo) -> Value {
        serde_json::from_slice(&self.compose(info).unwrap().payload).unwrap()
    }

    /// The paths of the `rerun-if-changed` directives, relative to the
    /// package directory.
    fn watched(&self, info: &BuildInfo) -> Vec<String> {
        self.compose(info)
            .unwrap()
            .directives
            .iter()
            .filter_map(|d| d.strip_prefix("rerun-if-changed="))
            .map(|p| {
                Path::new(p)
                    .strip_prefix(&self.dir)
                    .map_or(p.into(), |p| p.to_str().unwrap().into())
            })
            .collect()
    }
}

fn info() -> BuildInfo {
    BuildInfo::from_cargo_env()
}

#[test]
fn encode_round_trips() {
    let payload = br#"{"schema":1}"#;
    let image = encode(payload, 1024).unwrap();
    let section = crate::parse(&image).unwrap();
    assert_eq!(section.payload, payload);
    assert_eq!(section.region.len(), 1024);
    assert_eq!(section.post_link(), crate::PostLink::Erased);
    assert_eq!(image.len() % 8, 0);

    let image = encode(payload, 0).unwrap();
    assert_eq!(
        crate::parse(&image).unwrap().post_link(),
        crate::PostLink::None
    );
}

#[test]
fn encode_limits() {
    assert!(encode(&vec![b' '; PAYLOAD_MAX], 0).is_ok());
    assert!(encode(&vec![b' '; PAYLOAD_MAX + 1], 0).is_err());
    let fixed = 40; // header and `{}`, padded to 8
    assert!(encode(b"{}", SECTION_MAX - fixed).is_ok());
    assert!(encode(b"{}", SECTION_MAX - fixed + 1).is_err());
    assert!(encode(b"{}", usize::MAX).is_err());
}

#[test]
fn without_git_revision_and_dirty_are_null() {
    let pkg = Package::new();
    let p = pkg.payload(&info());
    assert_eq!(
        p,
        json!({
            "schema": 1, "name": "app", "version": "1.2.3", "revision": null,
            "dirty": null, "kind": null, "config_hash": null,
            "profile": "release", "target": "thumbv8m.main-none-eabihf",
        })
    );
    assert_eq!(pkg.watched(&info()), ["src", "Cargo.toml", "build.rs"]);
    let directives = pkg.compose(&info()).unwrap().directives;
    for var in [REVISION_VAR, DIRTY_VAR, INFO_VAR] {
        assert!(directives.contains(&format!("rerun-if-env-changed={var}")));
    }
}

#[test]
fn fields_in_schema_order_and_compact() {
    let pkg = Package::new();
    let payload = pkg.compose(&info().kind("release")).unwrap().payload;
    let text = String::from_utf8(payload).unwrap();
    assert!(text.starts_with(r#"{"schema":1,"name":"app","version":"1.2.3","revision":null,"#));
    assert!(!text.contains(' '));
    assert!(!text.contains("ext"), "`ext` only when set");
}

#[test]
fn setters() {
    let pkg = Package::new();
    let p = pkg.payload(
        &info()
            .kind("simulation")
            .config_hash("crc32:0a1b")
            .ext("board", "nucleo")
            .ext("rev", 3),
    );
    assert_eq!(p["kind"], "simulation");
    assert_eq!(p["config_hash"], "crc32:0a1b");
    assert_eq!(p["ext"], json!({ "board": "nucleo", "rev": 3 }));
    // Validated like an override.
    assert!(pkg.compose(&info().config_hash("SHA:xyz")).is_err());
}

#[test]
fn config_hash_of_file() {
    let pkg = Package::new();
    pkg.file("config/app.toml", "abc");
    let i = info().config_hash_of_file("config/app.toml");
    assert_eq!(
        pkg.payload(&i)["config_hash"],
        "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert!(pkg.watched(&i).contains(&"config/app.toml".into()));

    let missing = pkg.compose(&info().config_hash_of_file("nope.toml"));
    assert!(matches!(missing, Err(Error::Io(..))));
}

#[test]
fn identical_bytes_across_runs() {
    let pkg = Package::new();
    pkg.repo().file("a.txt", "a").commit();
    let i = info().kind("release").ext("x", 1);
    let a = pkg.compose(&i).unwrap();
    let b = pkg.compose(&i).unwrap();
    assert_eq!(a.payload, b.payload);
    assert_eq!(a.directives, b.directives);
    assert_eq!(
        encode(&a.payload, 64).unwrap(),
        encode(&b.payload, 64).unwrap()
    );
}

#[test]
fn git_revision_and_dirty() {
    let pkg = Package::new();
    pkg.repo().file("a.txt", "a").commit();
    let p = pkg.payload(&info());
    assert_eq!(p["revision"], pkg.git(&["rev-parse", "HEAD"]));
    assert_eq!(p["dirty"], false);

    let watched = pkg.watched(&info());
    assert!(watched.contains(&".git/HEAD".into()), "{watched:?}");
    let head_ref = pkg.git(&["symbolic-ref", "HEAD"]);
    assert!(watched.contains(&format!(".git/{head_ref}")), "{watched:?}");

    pkg.file("a.txt", "changed");
    assert_eq!(pkg.payload(&info())["dirty"], true);
    pkg.git(&["checkout", "--", "a.txt"]);
    pkg.file("new.txt", "untracked");
    assert_eq!(pkg.payload(&info())["dirty"], true);
}

#[test]
fn repository_without_commits() {
    let pkg = Package::new();
    pkg.repo().file("a.txt", "a");
    let p = pkg.payload(&info());
    assert_eq!(p["revision"], Value::Null);
    assert_eq!(p["dirty"], true);
}

#[test]
fn packed_ref_watches_its_directory() {
    let pkg = Package::new();
    pkg.repo().file("a.txt", "a").commit();
    pkg.git(&["pack-refs", "--all"]);
    let watched = pkg.watched(&info());
    assert!(watched.contains(&".git/packed-refs".into()), "{watched:?}");
    assert!(watched.contains(&".git/refs/heads".into()), "{watched:?}");
}

#[test]
fn tree_watch_takes_clean_directories_whole() {
    let pkg = Package::new();
    pkg.repo()
        .file(".gitignore", "target/\n*.log\n")
        .file("Cargo.toml", "")
        .file("assets/img/a.png", "")
        .file("crate/src/lib.rs", "")
        .file("crate/Cargo.toml", "")
        .commit()
        .file("crate/out.log", "ignored")
        .file("target/debug/x", "ignored")
        .file("notes.txt", "untracked");
    let mut watched = pkg.watched(&info());
    watched.sort();
    let head_ref = format!(".git/{}", pkg.git(&["symbolic-ref", "HEAD"]));
    assert_eq!(
        watched,
        [
            ".git/HEAD",
            ".git/index",
            &head_ref,
            ".gitignore",
            "Cargo.toml",
            "assets",
            "crate/Cargo.toml",
            "crate/src",
            "notes.txt",
        ]
        .map(String::from),
    );
}

#[test]
fn track_dirty_false() {
    let pkg = Package::new();
    pkg.repo()
        .file("assets/a", "")
        .commit()
        .file("assets/a", "x");
    let i = info().track_dirty(false).watch("assets");
    let p = pkg.payload(&i);
    assert_eq!(p["dirty"], Value::Null);
    assert!(p["revision"].is_string());
    let watched = pkg.watched(&i);
    for path in ["assets", ".git/HEAD", "src", "Cargo.toml", "build.rs"] {
        assert!(watched.contains(&path.into()), "{path} in {watched:?}");
    }
    assert!(!watched.contains(&".git/index".into()), "{watched:?}");
}

/// Staging alone makes the repository dirty without touching a file, so the
/// index is part of the dirty watch.
#[test]
fn staging_alone_is_dirty_and_the_index_is_watched() {
    let pkg = Package::new();
    pkg.repo().file("a.txt", "a").commit();
    assert_eq!(pkg.payload(&info())["dirty"], false);
    assert!(pkg.watched(&info()).contains(&".git/index".into()));

    pkg.git(&["update-index", "--chmod=+x", "a.txt"]);
    assert_eq!(pkg.payload(&info())["dirty"], true);
    pkg.git(&["update-index", "--chmod=-x", "a.txt"]);
    assert_eq!(pkg.payload(&info())["dirty"], false);
}

#[test]
fn revision_and_dirty_overrides() {
    let mut pkg = Package::new();
    pkg.repo().file("a.txt", "a").commit();
    pkg.set(REVISION_VAR, "abc123").set(DIRTY_VAR, "true");
    let p = pkg.payload(&info());
    assert_eq!(p["revision"], "abc123");
    assert_eq!(p["dirty"], true);
    // No git watch for overridden values; the package instead.
    let watched = pkg.watched(&info());
    assert!(!watched.contains(&".git/HEAD".into()), "{watched:?}");
    assert!(!watched.contains(&".git/index".into()), "{watched:?}");
    assert!(watched.contains(&"src".into()), "{watched:?}");

    pkg.set(DIRTY_VAR, "false");
    assert_eq!(pkg.payload(&info())["dirty"], false);
    pkg.set(DIRTY_VAR, "yes");
    assert!(matches!(pkg.compose(&info()), Err(Error::Invalid(_))));
    // Empty means unset.
    pkg.set(REVISION_VAR, "").set(DIRTY_VAR, "");
    let p = pkg.payload(&info());
    assert_eq!(p["revision"], pkg.git(&["rev-parse", "HEAD"]));
    assert_eq!(p["dirty"], false);
}

#[test]
fn info_override_is_compacted_and_validated() {
    let mut pkg = Package::new();
    let full = json!({
        "schema": 1, "name": "ci-app", "version": "9.0.0", "revision": "f00",
        "dirty": false, "kind": "release", "config_hash": null,
        "profile": "release", "target": "x", "ext": { "note": "two\nlines" },
        "future": [1, 2],
    });
    pkg.file("info.json", &serde_json::to_string_pretty(&full).unwrap());
    pkg.set(INFO_VAR, "info.json");
    let composed = pkg.compose(&info().kind("ignored")).unwrap();
    assert!(payload_is_single_line(&composed.payload));
    assert_eq!(
        serde_json::from_slice::<Value>(&composed.payload).unwrap(),
        full
    );
    assert!(composed.directives.contains(&format!(
        "rerun-if-changed={}",
        pkg.dir.join("info.json").display()
    )));

    let mut missing = full.clone();
    missing.as_object_mut().unwrap().remove("revision");
    pkg.file("info.json", &missing.to_string());
    assert!(matches!(pkg.compose(&info()), Err(Error::Invalid(_))));

    let mut big = full.clone();
    big["ext"] = json!({ "pad": "x".repeat(PAYLOAD_MAX) });
    pkg.file("info.json", &big.to_string());
    assert!(matches!(pkg.compose(&info()), Err(Error::Invalid(_))));

    pkg.file("info.json", "{ not json");
    assert!(matches!(pkg.compose(&info()), Err(Error::Invalid(_))));
    pkg.set(INFO_VAR, "absent.json");
    assert!(matches!(pkg.compose(&info()), Err(Error::Io(..))));
}

#[test]
fn validate_schema_1() {
    let ok = json!({
        "schema": 1, "name": "a", "version": "1", "revision": null, "dirty": null,
        "kind": null, "config_hash": "sha256:00ff", "profile": "p", "target": "t",
    });
    validate(&serde_json::to_vec(&ok).unwrap()).unwrap();

    for (key, value) in [
        ("schema", json!(2)),
        ("schema", json!("1")),
        ("name", Value::Null),
        ("version", json!(1)),
        ("profile", json!(null)),
        ("target", json!([])),
        ("revision", json!(1)),
        ("kind", json!(false)),
        ("dirty", json!("yes")),
        ("config_hash", json!("sha256")),
        ("config_hash", json!("sha256:")),
        ("config_hash", json!(":00")),
        ("config_hash", json!("sha256:00FF")),
        ("config_hash", json!("SHA256:00")),
        ("ext", json!([])),
    ] {
        let mut p = ok.clone();
        p[key] = value.clone();
        assert!(
            validate(&serde_json::to_vec(&p).unwrap()).is_err(),
            "{key}: {value}"
        );
    }
    for key in ["schema", "name", "revision", "dirty", "kind", "config_hash"] {
        let mut p = ok.clone();
        p.as_object_mut().unwrap().remove(key);
        assert!(validate(&serde_json::to_vec(&p).unwrap()).is_err(), "{key}");
    }
    assert!(validate(b"[1]").is_err());
    assert!(validate(&serde_json::to_vec_pretty(&ok).unwrap()).is_err());
}
