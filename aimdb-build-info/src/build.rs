//! The build-script helper (feature `build`): composes the payload and writes
//! the section image to `OUT_DIR`.

use std::borrow::ToOwned;
use std::collections::BTreeSet;
use std::format;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::string::{String, ToString};
use std::vec::Vec;
use std::{env, fs, io, println, vec};

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::{
    payload_is_single_line, ERASED, FILE_NAME, FORMAT, HEADER_LEN, MAGIC, PAYLOAD_MAX, SCHEMA,
    SECTION_MAX,
};

/// Size of the post-link region unless the build script sets one.
pub const DEFAULT_POST_LINK_LEN: usize = 1024;

/// Replaces the git revision.
const REVISION_VAR: &str = "AIMDB_BUILD_REVISION";
/// Replaces the git `dirty` flag: `true` or `false`.
const DIRTY_VAR: &str = "AIMDB_BUILD_DIRTY";
/// Names a file holding the complete payload.
const INFO_VAR: &str = "AIMDB_BUILD_INFO";

/// Why the build info could not be written.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// A variable Cargo sets for build scripts is missing.
    MissingEnv(&'static str),
    /// Reading an input or writing the section failed.
    Io(PathBuf, io::Error),
    /// An override, the payload or the section does not fit schema 1 or the
    /// layout's limits.
    Invalid(String),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::MissingEnv(v) => write!(f, "{v} is not set; call this from a build script"),
            Self::Io(path, e) => write!(f, "{}: {e}", path.display()),
            Self::Invalid(m) => write!(f, "build info: {m}"),
        }
    }
}

impl std::error::Error for Error {}

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}

#[derive(Debug, Clone)]
enum ConfigHash {
    Given(String),
    OfFile(PathBuf),
}

/// Composes the payload in a build script and writes the section image to
/// `OUT_DIR`, where `section!()` picks it up.
///
/// ```no_run
/// // build.rs
/// aimdb_build_info::BuildInfo::from_cargo_env()
///     .kind("release")
///     .config_hash_of_file("config/gateway.toml")
///     .ext("board", "nucleo-h563zi")
///     .write()
///     .expect("build info");
/// ```
///
/// The payload holds no time, host or user, so the same source,
/// configuration and toolchain give the same bytes. Setters only record their
/// inputs; [`write`](Self::write) reads files, runs git and reports errors.
///
/// # Overrides
///
/// - `AIMDB_BUILD_REVISION` and `AIMDB_BUILD_DIRTY` (`true`/`false`) replace
///   the git values, for builds without `.git`.
/// - `AIMDB_BUILD_INFO=<path>` uses that file as the complete payload. It is
///   re-serialized compactly and must fit schema 1; a relative path is taken
///   from the package directory.
///
/// # Rerun tracking
///
/// The build script reruns when HEAD or its branch moves, when an override
/// changes, and when an input named by [`config_hash_of_file`] or
/// [`watch`](Self::watch) changes. Because `dirty` describes the whole
/// repository, it also reruns when any file git knows changes or the index
/// changes: a directory with nothing ignored below it is watched whole, so a
/// new file there counts; elsewhere the known files are watched one by one.
/// Refreshing the index, as a plain `git status` does, costs one rerun. A new
/// untracked file in a directory that also holds ignored output (such as the
/// repository top, beside `target/`) is not noticed; name such inputs with
/// [`watch`](Self::watch).
///
/// [`config_hash_of_file`]: Self::config_hash_of_file
#[derive(Debug, Clone)]
pub struct BuildInfo {
    kind: Option<String>,
    config_hash: Option<ConfigHash>,
    ext: Map<String, Value>,
    post_link_len: usize,
    watch: Vec<PathBuf>,
    track_dirty: bool,
}

impl BuildInfo {
    /// Name, version, profile and target from Cargo; revision and `dirty`
    /// from git, or `null` without git. Nothing is read until
    /// [`write`](Self::write).
    pub fn from_cargo_env() -> Self {
        Self {
            kind: None,
            config_hash: None,
            ext: Map::new(),
            post_link_len: DEFAULT_POST_LINK_LEN,
            watch: Vec::new(),
            track_dirty: true,
        }
    }

    /// The build kind, e.g. `release` or `simulation`.
    pub fn kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = Some(kind.into());
        self
    }

    /// The configuration hash as given, `"<alg>:<hex>"`.
    pub fn config_hash(mut self, hash: impl Into<String>) -> Self {
        self.config_hash = Some(ConfigHash::Given(hash.into()));
        self
    }

    /// The configuration hash as `sha256:<hex>` of a file's bytes; `path` is
    /// taken from the package directory. The file is watched.
    pub fn config_hash_of_file(mut self, path: impl AsRef<Path>) -> Self {
        self.config_hash = Some(ConfigHash::OfFile(path.as_ref().to_owned()));
        self
    }

    /// An application field under `ext`.
    pub fn ext(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.ext.insert(key.into(), value.into());
        self
    }

    /// Size of the post-link region in bytes; 0 for none. Default 1 KiB.
    pub fn post_link_len(mut self, len: usize) -> Self {
        self.post_link_len = len;
        self
    }

    /// Rerun the build script when `path` changes: a file, or a directory
    /// watched recursively. `path` is taken from the package directory.
    pub fn watch(mut self, path: impl AsRef<Path>) -> Self {
        self.watch.push(path.as_ref().to_owned());
        self
    }

    /// Whether to compute `dirty` (default `true`). Tracking reruns the build
    /// script whenever a file in the repository changes. With `false`,
    /// `dirty` is `null`, and only the package's `src/`, `Cargo.toml` and
    /// `build.rs` are watched besides the revision, overrides and inputs.
    pub fn track_dirty(mut self, track: bool) -> Self {
        self.track_dirty = track;
        self
    }

    /// Write `OUT_DIR/aimdb_build_info.bin`, print the rerun directives, and
    /// return the file's path.
    pub fn write(self) -> Result<PathBuf, Error> {
        let env = |k: &str| env::var(k).ok();
        let composed = self.compose(&env)?;
        for directive in &composed.directives {
            println!("cargo:{directive}");
        }
        let out = env("OUT_DIR").ok_or(Error::MissingEnv("OUT_DIR"))?;
        let path = PathBuf::from(out).join(FILE_NAME);
        let image = encode(&composed.payload, self.post_link_len)?;
        fs::write(&path, image).map_err(|e| Error::Io(path.clone(), e))?;
        Ok(path)
    }

    /// The payload and the rerun directives, with every variable read through
    /// `env`.
    fn compose(&self, env: &dyn Fn(&str) -> Option<String>) -> Result<Composed, Error> {
        let var = |k: &'static str| env(k).ok_or(Error::MissingEnv(k));
        let set = |k: &str| env(k).filter(|v| !v.is_empty());
        let dir = PathBuf::from(var("CARGO_MANIFEST_DIR")?);

        let mut watch = Watch::default();
        for v in [REVISION_VAR, DIRTY_VAR, INFO_VAR] {
            watch.env(v);
        }
        for path in &self.watch {
            watch.path(dir.join(path));
        }

        if let Some(file) = set(INFO_VAR) {
            let path = dir.join(file);
            watch.path(path.clone());
            let raw = fs::read(&path).map_err(|e| Error::Io(path.clone(), e))?;
            let value: Value = serde_json::from_slice(&raw)
                .map_err(|e| invalid(format!("{}: {e}", path.display())))?;
            let payload = serde_json::to_vec(&value).map_err(|e| invalid(e.to_string()))?;
            validate(&payload)?;
            return Ok(Composed {
                payload,
                directives: watch.directives,
            });
        }

        let git = Git::find(&dir);
        let revision = match (set(REVISION_VAR), &git) {
            (Some(r), _) => Value::String(r),
            (None, Some(git)) => {
                git.watch_head(&mut watch);
                git.revision().map_or(Value::Null, Value::String)
            }
            (None, None) => Value::Null,
        };

        let dirty_override = match set(DIRTY_VAR).as_deref() {
            None => None,
            Some("true") => Some(true),
            Some("false") => Some(false),
            Some(other) => {
                return Err(invalid(format!(
                    "{DIRTY_VAR} must be `true` or `false`, not `{other}`"
                )))
            }
        };
        let mut dirty = dirty_override.map_or(Value::Null, Value::Bool);
        if let (None, true, Some(git)) = (dirty_override, self.track_dirty, &git) {
            // Without the watch the value would go stale: report unknown.
            if let Some(d) = git.dirty() {
                if git.watch_tree(&mut watch) {
                    dirty = Value::Bool(d);
                }
            }
        }
        if !watch.tree {
            watch.package(&dir);
        }

        let config_hash = match &self.config_hash {
            None => Value::Null,
            Some(ConfigHash::Given(hash)) => Value::String(hash.clone()),
            Some(ConfigHash::OfFile(file)) => {
                let path = dir.join(file);
                watch.path(path.clone());
                let bytes = fs::read(&path).map_err(|e| Error::Io(path.clone(), e))?;
                Value::String(format!("sha256:{}", hex(&Sha256::digest(&bytes))))
            }
        };

        let mut fields = vec![
            ("schema", Value::from(SCHEMA)),
            ("name", Value::String(var("CARGO_PKG_NAME")?)),
            ("version", Value::String(var("CARGO_PKG_VERSION")?)),
            ("revision", revision),
            ("dirty", dirty),
            ("kind", self.kind.clone().map_or(Value::Null, Value::String)),
            ("config_hash", config_hash),
            ("profile", Value::String(var("PROFILE")?)),
            ("target", Value::String(var("TARGET")?)),
        ];
        if !self.ext.is_empty() {
            fields.push(("ext", Value::Object(self.ext.clone())));
        }
        let payload = object(&fields)?;
        validate(&payload)?;
        Ok(Composed {
            payload,
            directives: watch.directives,
        })
    }
}

struct Composed {
    payload: Vec<u8>,
    directives: Vec<String>,
}

/// A compact JSON object with the fields in the given order. (A
/// `serde_json::Map` sorts its keys.)
fn object(fields: &[(&str, Value)]) -> Result<Vec<u8>, Error> {
    let mut out = vec![b'{'];
    for (i, (key, value)) in fields.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        serde_json::to_writer(&mut out, key).map_err(|e| invalid(e.to_string()))?;
        out.push(b':');
        serde_json::to_writer(&mut out, value).map_err(|e| invalid(e.to_string()))?;
    }
    out.push(b'}');
    Ok(out)
}

/// Check a payload against schema 1: size, no line break, a JSON object, and
/// the standard fields' types.
fn validate(payload: &[u8]) -> Result<(), Error> {
    if payload.len() > PAYLOAD_MAX {
        return Err(invalid(format!(
            "the payload is {} bytes, at most {PAYLOAD_MAX}",
            payload.len()
        )));
    }
    if !payload_is_single_line(payload) {
        return Err(invalid("the payload contains a line break"));
    }
    let value: Value = serde_json::from_slice(payload).map_err(|e| invalid(e.to_string()))?;
    let Some(o) = value.as_object() else {
        return Err(invalid("the payload is not a JSON object"));
    };
    let field = |k: &str| o.get(k).unwrap_or(&Value::Null);
    if field("schema").as_u64() != Some(SCHEMA) {
        return Err(invalid(format!("`schema` must be {SCHEMA}")));
    }
    for k in ["name", "version", "profile", "target"] {
        if !field(k).is_string() {
            return Err(invalid(format!("`{k}` must be a string")));
        }
    }
    for k in ["revision", "kind"] {
        if !(o.contains_key(k) && (field(k).is_string() || field(k).is_null())) {
            return Err(invalid(format!("`{k}` must be a string or null")));
        }
    }
    if !(o.contains_key("dirty") && (field("dirty").is_boolean() || field("dirty").is_null())) {
        return Err(invalid("`dirty` must be a bool or null"));
    }
    match o.get("config_hash") {
        Some(Value::Null) => {}
        Some(Value::String(h)) if is_config_hash(h) => {}
        _ => return Err(invalid("`config_hash` must be \"<alg>:<hex>\" or null")),
    }
    if o.get("ext").is_some_and(|ext| !ext.is_object()) {
        return Err(invalid("`ext` must be an object"));
    }
    Ok(())
}

/// `<alg>:<hex>`: an algorithm name of lowercase letters, digits and `-`, and
/// lowercase hex.
fn is_config_hash(hash: &str) -> bool {
    let Some((alg, hex)) = hash.split_once(':') else {
        return false;
    };
    !alg.is_empty()
        && alg
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !hex.is_empty()
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The section image for `payload`, with an erased post-link region of
/// `post_link_len` bytes.
pub fn encode(payload: &[u8], post_link_len: usize) -> Result<Vec<u8>, Error> {
    if payload.len() > PAYLOAD_MAX {
        return Err(invalid(format!(
            "the payload is {} bytes, at most {PAYLOAD_MAX}",
            payload.len()
        )));
    }
    let region_off = (HEADER_LEN + payload.len()).div_ceil(8) * 8;
    let total_len = region_off
        .checked_add(post_link_len)
        .filter(|&t| t <= SECTION_MAX)
        .ok_or_else(|| {
            invalid(format!(
                "a {post_link_len}-byte post-link region makes the section larger than {SECTION_MAX} bytes"
            ))
        })?;
    // Below SECTION_MAX, so every length fits its field.
    let u32_of = |n: usize| (n as u32).to_le_bytes();
    let mut out = vec![0u8; total_len];
    out[..8].copy_from_slice(&MAGIC);
    out[8..10].copy_from_slice(&FORMAT.to_le_bytes());
    out[10..12].copy_from_slice(&(HEADER_LEN as u16).to_le_bytes());
    out[12..16].copy_from_slice(&u32_of(payload.len()));
    out[16..20].copy_from_slice(&u32_of(region_off));
    out[20..24].copy_from_slice(&u32_of(post_link_len));
    out[24..28].copy_from_slice(&u32_of(total_len));
    out[HEADER_LEN..HEADER_LEN + payload.len()].copy_from_slice(payload);
    out[region_off..].fill(ERASED);
    Ok(out)
}

/// Lowercase hex.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The rerun directives, each path once.
#[derive(Default)]
struct Watch {
    directives: Vec<String>,
    seen: BTreeSet<PathBuf>,
    /// Whether the repository-wide watch is in place.
    tree: bool,
}

impl Watch {
    fn env(&mut self, var: &str) {
        self.directives.push(format!("rerun-if-env-changed={var}"));
    }

    fn path(&mut self, path: PathBuf) {
        if !self.seen.contains(&path) {
            self.directives
                .push(format!("rerun-if-changed={}", path.display()));
            self.seen.insert(path);
        }
    }

    /// What Cargo would watch without directives, roughly: the package's
    /// sources and manifest.
    fn package(&mut self, dir: &Path) {
        for f in ["src", "Cargo.toml", "build.rs"] {
            self.path(dir.join(f));
        }
    }
}

/// git, run in the package directory; every failure means "unknown".
struct Git {
    dir: PathBuf,
    top: PathBuf,
}

impl Git {
    /// The repository the package directory is in, if git runs and finds one.
    fn find(dir: &Path) -> Option<Self> {
        let top = run(dir, &["rev-parse", "--show-toplevel"])?;
        Some(Self {
            dir: dir.to_owned(),
            top: PathBuf::from(top),
        })
    }

    /// `None` in a repository without commits.
    fn revision(&self) -> Option<String> {
        run(&self.dir, &["rev-parse", "HEAD"])
    }

    /// `--no-optional-locks`: a plain `git status` refreshes and rewrites the
    /// index, and the index is watched, so the build script would rerun after
    /// every build because of its own check.
    fn dirty(&self) -> Option<bool> {
        run(&self.dir, &["--no-optional-locks", "status", "--porcelain"]).map(|s| !s.is_empty())
    }

    /// Watch HEAD, the ref it points to and `packed-refs`, which together
    /// give the revision.
    fn watch_head(&self, watch: &mut Watch) {
        // `--git-path` answers relative to the working directory (and
        // `--path-format=absolute` needs git 2.31).
        let git_path =
            |p: &str| run(&self.dir, &["rev-parse", "--git-path", p]).map(|r| self.dir.join(r));
        for p in ["HEAD", "packed-refs"] {
            if let Some(path) = git_path(p).filter(|path| path.exists()) {
                watch.path(path);
            }
        }
        let Some(head_ref) = run(&self.dir, &["symbolic-ref", "-q", "HEAD"]) else {
            return; // detached: HEAD holds the revision itself
        };
        let Some(path) = git_path(&head_ref) else {
            return;
        };
        if path.exists() {
            watch.path(path);
        } else if let Some(dir) = path.parent().filter(|d| d.exists()) {
            // A packed ref has no file until the next commit writes one.
            watch.path(dir.to_owned());
        }
    }

    /// Watch every file git knows (tracked, or untracked and not ignored),
    /// and the index. Returns whether the watch is in place.
    ///
    /// Cargo watches a directory recursively, so the top-most directory with
    /// no ignored path below it is watched whole and a new file anywhere in
    /// it counts. Elsewhere, and always at the top (which holds `.git/`), the
    /// known files are watched one by one.
    ///
    /// The index, because staging alone makes the repository dirty without
    /// touching a file: `git update-index --chmod=+x`, or `git add` of an
    /// edit already built. Other git commands that refresh the index, such as
    /// a plain `git status`, cause one extra rerun.
    fn watch_tree(&self, watch: &mut Watch) -> bool {
        // From the top: in a subdirectory, `ls-files` lists only that
        // subdirectory.
        let ls = |extra: &[&str]| {
            let mut args = vec!["ls-files", "-z", "--exclude-standard"];
            args.extend_from_slice(extra);
            run(&self.top, &args)
        };
        let (Some(known), Some(ignored)) = (
            ls(&["--cached", "--others"]),
            ls(&["--others", "--ignored", "--directory"]),
        ) else {
            return false;
        };

        // Every directory with an ignored path below it, and the top.
        let mut unclean = BTreeSet::from([""]);
        for path in ignored.split('\0') {
            let mut path = path.trim_end_matches('/');
            while let Some((parent, _)) = path.rsplit_once('/') {
                unclean.insert(parent);
                path = parent;
            }
        }

        for file in known.split('\0').filter(|f| !f.is_empty()) {
            let clean_dir = file
                .match_indices('/')
                .map(|(i, _)| &file[..i])
                .find(|dir| !unclean.contains(dir));
            let path = self.top.join(clean_dir.unwrap_or(file));
            // A deleted tracked file would rerun the script on every build.
            if path.exists() {
                watch.path(path);
            }
        }
        // Relative to the package directory, like `watch_head`'s paths. A
        // repository with nothing staged yet has no index.
        let index = run(&self.dir, &["rev-parse", "--git-path", "index"]).map(|p| self.dir.join(p));
        if let Some(index) = index.filter(|p| p.exists()) {
            watch.path(index);
        }
        watch.tree = true;
        true
    }
}

/// Run git in `dir`; its stdout without the final newline, if it succeeds.
fn run(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    out.status.success().then(|| {
        String::from_utf8_lossy(&out.stdout)
            .trim_end_matches('\n')
            .to_owned()
    })
}

#[cfg(test)]
mod tests;
