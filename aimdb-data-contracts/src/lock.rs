//! `aimdb.lock`: the wire shape of every contract version, checked by the
//! test that [`catalog!`](crate::catalog) generates (feature `lock`).
//!
//! One rule: **the same `NAME@VERSION` always has the same wire shape.**
//!
//! | Change | Result |
//! |---|---|
//! | New contract or new `VERSION` | fails until accepted with `AIMDB_UPDATE_LOCK=1` |
//! | Contract removed from the catalog | fails until accepted with `AIMDB_UPDATE_LOCK=1` |
//! | Shape changed under the same `NAME@VERSION` | always fails |
//! | Two catalog entries with the same `NAME@VERSION` | always fails |
//! | A contract whose shape cannot be traced | always fails |
//!
//! A locked entry never changes, so the lock holds only a hash of each
//! shape (its format is in `lock/shape.rs`). A failure shows the current shape.

mod shape;

pub use shape::{helper, Helper};

use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::SchemaType;

/// Version of the lock file and of the shape text it hashes.
const FORMAT_VERSION: u32 = 1;

const FILE_NAME: &str = "aimdb.lock";

const HEADER: &str =
    "# aimdb.lock: generated, do not edit. AIMDB_UPDATE_LOCK=1 cargo test to update.\n";

/// One catalog entry with its traced shape.
#[derive(Clone, Debug)]
pub struct Traced {
    name: &'static str,
    version: u32,
    /// Shape text, or why it cannot be traced.
    shape: Result<String, String>,
}

/// Trace the wire shape of contract `T`, after `helpers`. Never panics: a
/// contract outside the supported profile is reported by [`check`]
/// together with everything else.
pub fn trace<T: SchemaType + Serialize + DeserializeOwned>(helpers: &[Helper]) -> Traced {
    Traced {
        name: T::NAME,
        version: T::VERSION,
        shape: shape::shape_of::<T>(helpers),
    }
}

/// Check `current` against `<crate_root>/aimdb.lock`; with
/// `AIMDB_UPDATE_LOCK=1`, write accepted changes to it.
///
/// # Panics
///
/// On any finding that is not accepted, listing all of them.
pub fn check(crate_root: &str, current: &[Traced]) {
    let update = std::env::var("AIMDB_UPDATE_LOCK").is_ok_and(|v| v == "1");
    match check_with(Path::new(crate_root), current, update) {
        Ok(Some(changes)) => std::eprintln!("{FILE_NAME} updated:\n{changes}"),
        Ok(None) => {}
        Err(report) => panic!("\n{report}"),
    }
}

/// [`check`] without the environment: `Ok(Some(changes))` when the lock was
/// written.
fn check_with(root: &Path, current: &[Traced], update: bool) -> Result<Option<String>, String> {
    let path = root.join(FILE_NAME);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let locked = text.as_deref().map(parse).transpose()?;
    let report = diff(locked.as_ref(), current);

    let mut out = String::new();
    for failure in &report.failures {
        let _ = writeln!(out, "{failure}");
    }
    let changes = report.changes();
    let rendered = render(&report.entries);
    let drift = changes.is_empty() && text.as_deref() != Some(rendered.as_str());

    if !report.failures.is_empty() {
        if !changes.is_empty() {
            let _ = writeln!(out, "{FILE_NAME} would also change:\n{changes}");
        }
        return Err(out);
    }
    if report.stale_format && !update {
        let _ = writeln!(
            out,
            "{FILE_NAME} has format version {}, this aimdb-data-contracts writes {FORMAT_VERSION}\n  \
             fix: regenerate it with AIMDB_UPDATE_LOCK=1 cargo test",
            locked.map_or(0, |l| l.version)
        );
        return Err(out);
    }
    if changes.is_empty() && !drift {
        return Ok(None);
    }
    if !update {
        let _ = writeln!(out, "{FILE_NAME} is out of date:");
        if drift {
            let _ = writeln!(out, "  (not in its generated form)");
        }
        out.push_str(&changes);
        let _ = writeln!(out, "  fix: accept with AIMDB_UPDATE_LOCK=1 cargo test");
        return Err(out);
    }
    std::fs::write(&path, rendered).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Some(if drift {
        "  (regenerated)\n".into()
    } else {
        changes
    }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Lock {
    version: u32,
    #[serde(default)]
    contract: Vec<Entry>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
struct Entry {
    name: String,
    version: u32,
    shape: String,
}

fn parse(text: &str) -> Result<Lock, String> {
    toml::from_str(text).map_err(|e| format!("{FILE_NAME} cannot be read: {e}"))
}

/// What the lock says against what the catalog has.
#[derive(Debug, Default)]
struct Report {
    /// Findings no update accepts.
    failures: Vec<String>,
    added: Vec<Entry>,
    removed: Vec<Entry>,
    /// The lock was written by another format version: every entry is
    /// replaced.
    stale_format: bool,
    /// The lock as it should be, sorted by name and version.
    entries: Vec<Entry>,
}

impl Report {
    fn changes(&self) -> String {
        let mut out = String::new();
        for (sign, entries) in [("+", &self.added), ("-", &self.removed)] {
            for e in entries {
                let _ = writeln!(out, "  {sign} {}@{}  {}", e.name, e.version, e.shape);
            }
        }
        out
    }
}

fn diff(locked: Option<&Lock>, current: &[Traced]) -> Report {
    let mut report = Report::default();

    let mut locked_map = BTreeMap::new();
    match locked {
        Some(lock) if lock.version != FORMAT_VERSION => report.stale_format = true,
        Some(lock) => {
            for e in &lock.contract {
                let key = (e.name.as_str(), e.version);
                if locked_map.insert(key, e).is_some() {
                    report.failures.push(format!(
                        "{}@{}: locked twice in {FILE_NAME}\n  \
                         fix: restore {FILE_NAME} from version control",
                        e.name, e.version
                    ));
                }
            }
        }
        None => {}
    }

    let mut seen = BTreeMap::new();
    for t in current {
        if seen.insert((t.name, t.version), ()).is_some() {
            report.failures.push(format!(
                "{}@{}: listed twice in the catalog\n  \
                 fix: give each contract its own NAME, or bump VERSION",
                t.name, t.version
            ));
            continue;
        }
        let text = match &t.shape {
            Ok(text) => text,
            Err(reason) => {
                report.failures.push(format!(
                    "{}@{}: the shape cannot be traced: {reason}",
                    t.name, t.version
                ));
                continue;
            }
        };
        let entry = Entry {
            name: t.name.into(),
            version: t.version,
            shape: shape::hash(text),
        };
        match locked_map.remove(&(t.name, t.version)) {
            Some(old) if old.shape != entry.shape => {
                report.failures.push(format!(
                    "{}@{}: the shape changed without a VERSION bump\n  \
                     locked:  {}\n  \
                     current: {}  {text}\n  \
                     fix: bump VERSION (and add a migration step), or revert the change",
                    t.name, t.version, old.shape, entry.shape
                ));
                // A locked entry never changes.
                report.entries.push(old.clone());
                continue;
            }
            Some(_) => {}
            None if report.stale_format => {}
            None => report.added.push(entry.clone()),
        }
        report.entries.push(entry);
    }
    report.removed = locked_map.into_values().cloned().collect();

    report.entries.sort();
    report.added.sort();
    report.removed.sort();
    report
}

fn render(entries: &[Entry]) -> String {
    let mut out = String::from(HEADER);
    let _ = writeln!(out, "version = {FORMAT_VERSION}");
    for e in entries {
        let _ = write!(
            out,
            "\n[[contract]]\nname = {}\nversion = {}\nshape = {}\n",
            quote(&e.name),
            e.version,
            quote(&e.shape)
        );
    }
    out
}

/// A TOML basic string.
fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn traced(name: &'static str, version: u32, shape: &str) -> Traced {
        Traced {
            name,
            version,
            shape: Ok(shape.into()),
        }
    }

    fn entry(name: &str, version: u32, shape: &str) -> Entry {
        Entry {
            name: name.into(),
            version,
            shape: shape::hash(shape),
        }
    }

    fn lock(entries: Vec<Entry>) -> Lock {
        Lock {
            version: FORMAT_VERSION,
            contract: entries,
        }
    }

    #[test]
    fn unchanged_is_clean() {
        let l = lock(vec![entry("a", 1, "u8")]);
        let r = diff(Some(&l), &[traced("a", 1, "u8")]);
        assert!(r.failures.is_empty() && r.added.is_empty() && r.removed.is_empty());
    }

    #[test]
    fn new_contract_and_new_version_are_added() {
        let l = lock(vec![entry("a", 1, "u8")]);
        let r = diff(
            Some(&l),
            &[
                traced("a", 1, "u8"),
                traced("a", 2, "u16"),
                traced("b", 1, "str"),
            ],
        );
        assert!(r.failures.is_empty());
        assert_eq!(r.added, vec![entry("a", 2, "u16"), entry("b", 1, "str")]);
        assert_eq!(r.entries.len(), 3);
    }

    #[test]
    fn missing_lock_adds_everything() {
        let r = diff(None, &[traced("a", 1, "u8")]);
        assert_eq!(r.added, vec![entry("a", 1, "u8")]);
    }

    #[test]
    fn dropped_contract_is_removed() {
        let l = lock(vec![entry("a", 1, "u8"), entry("a", 2, "u16")]);
        let r = diff(Some(&l), &[traced("a", 2, "u16")]);
        assert!(r.failures.is_empty());
        assert_eq!(r.removed, vec![entry("a", 1, "u8")]);
        assert_eq!(r.entries, vec![entry("a", 2, "u16")]);
    }

    #[test]
    fn changed_shape_fails_and_keeps_the_locked_entry() {
        let l = lock(vec![entry("a", 1, "u8")]);
        let r = diff(Some(&l), &[traced("a", 1, "u16")]);
        assert_eq!(r.failures.len(), 1);
        assert!(r.failures[0].contains("a@1: the shape changed without a VERSION bump"));
        assert!(r.failures[0].contains("  u16"));
        assert_eq!(r.entries, vec![entry("a", 1, "u8")]);
    }

    #[test]
    fn duplicate_catalog_entry_fails() {
        let r = diff(None, &[traced("a", 1, "u8"), traced("a", 1, "u8")]);
        assert_eq!(r.failures.len(), 1);
        assert!(r.failures[0].contains("listed twice"));
    }

    #[test]
    fn untraceable_fails_with_the_reason() {
        let t = Traced {
            name: "a",
            version: 1,
            shape: Err("Not supported: deserialize_identifier".into()),
        };
        let r = diff(None, &[t]);
        assert!(r.failures[0].contains("a@1: the shape cannot be traced: Not supported"));
    }

    #[test]
    fn other_format_version_replaces_everything() {
        let mut l = lock(vec![entry("a", 1, "old")]);
        l.version = FORMAT_VERSION + 1;
        let r = diff(Some(&l), &[traced("a", 1, "u8")]);
        assert!(r.stale_format && r.failures.is_empty() && r.added.is_empty());
        assert_eq!(r.entries, vec![entry("a", 1, "u8")]);
    }

    #[test]
    fn render_parse_roundtrip() {
        let entries = vec![entry("a\"\\", 1, "u8"), entry("b", 2, "str")];
        let text = render(&entries);
        let parsed = parse(&text).unwrap();
        assert_eq!(parsed.version, FORMAT_VERSION);
        assert_eq!(render(&parsed.contract), text);
    }

    fn temp_root(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("aimdb-lock-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn update_writes_additions_and_never_a_changed_shape() {
        let root = temp_root("update");
        let v1 = [traced("a", 1, "u8")];

        let err = check_with(&root, &v1, false).unwrap_err();
        assert!(
            err.contains("out of date") && err.contains("+ a@1"),
            "{err}"
        );

        assert!(check_with(&root, &v1, true).unwrap().is_some());
        assert_eq!(check_with(&root, &v1, false).unwrap(), None);

        let changed = [traced("a", 1, "u16")];
        assert!(check_with(&root, &changed, true).is_err());
        assert_eq!(check_with(&root, &v1, false).unwrap(), None);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hand_edited_lock_is_out_of_date() {
        let root = temp_root("edited");
        let v1 = [traced("a", 1, "u8")];
        check_with(&root, &v1, true).unwrap();
        let path = root.join(FILE_NAME);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace(HEADER, "")).unwrap();

        assert!(check_with(&root, &v1, false).is_err());
        check_with(&root, &v1, true).unwrap();
        assert_eq!(check_with(&root, &v1, false).unwrap(), None);

        let _ = std::fs::remove_dir_all(&root);
    }
}
