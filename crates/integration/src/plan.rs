//! What an item does to the system (`Op`s), carried out with a record of each change
//! (`Change`s, kept in the manifest), so that turning it off undoes exactly those.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::env::Env;
use crate::registry::{Root, Value};

/// One step of turning an item on.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Op {
    File {
        path: PathBuf,
        contents: Vec<u8>,
        executable: bool,
    },
    /// An `<action>` merged into Thunar's `uca.xml`; `template` is the system file a new
    /// user file starts from (Thunar reads only one of them).
    XmlAction {
        path: PathBuf,
        template: Option<PathBuf>,
        id: String,
        block: String,
    },
    Reg {
        root: Root,
        key: String,
        name: String,
        value: RegValue,
    },
    /// A key in the app's own namespace (its ProgID, class ID, verb): removed with
    /// everything below it.
    OwnKey { root: Root, key: String },
    /// This app (`app`, as the system names it) opens these types by default.
    Default { types: Vec<String>, app: String },
    /// `update-desktop-database DIR` after changes (and after undoing them).
    UpdateDesktopDatabase(PathBuf),
}

/// A registry value in an `Op` (hashable).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RegValue {
    String(String),
    Dword(u32),
}

impl From<&RegValue> for Value {
    fn from(v: &RegValue) -> Value {
        match v {
            RegValue::String(s) => Value::String(s.clone()),
            RegValue::Dword(d) => Value::Dword(*d),
        }
    }
}

/// What was done, as recorded.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Change {
    File {
        path: PathBuf,
        hash: u64,
    },
    /// A folder made for a file; removed again when empty.
    Dir {
        path: PathBuf,
    },
    XmlAction {
        path: PathBuf,
        id: String,
        /// The user file did not exist: it started as this copy of the template (or empty).
        created_from: Option<u64>,
    },
    Reg {
        root: Root,
        key: String,
        name: String,
        value: Value,
        previous: Option<Value>,
    },
    OwnKey {
        root: Root,
        key: String,
    },
    /// A key made on the way to a value; removed again when empty.
    CreatedKey {
        root: Root,
        key: String,
    },
    Default {
        ty: String,
        app: String,
        previous: Option<String>,
    },
    UpdateDesktopDatabase(PathBuf),
}

/// One item's record.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// Of the plan carried out: a different plan now (the app moved) means "repair".
    pub fingerprint: u64,
    pub changes: Vec<Change>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub items: BTreeMap<String, Record>,
    /// Folders and keys made on the way to an item's files or values that still held
    /// another item's when it was turned off: removed once empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub leftovers: Vec<Change>,
}

impl Manifest {
    pub fn load(path: &Path) -> Manifest {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if self.items.is_empty() && self.leftovers.is_empty() {
            return match std::fs::remove_file(path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)
    }
}

pub fn hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub fn fingerprint(ops: &[Op]) -> u64 {
    hash(format!("{ops:?}").as_bytes())
}

/// Carries out `ops`, recording each change as it is made (a failure leaves the record of
/// what was done, so it can be undone).
pub fn apply(env: &mut Env, ops: &[Op], rec: &mut Record) -> io::Result<()> {
    rec.fingerprint = fingerprint(ops);
    for op in ops {
        match op {
            Op::File {
                path,
                contents,
                executable,
            } => {
                make_dirs(path.parent(), rec)?;
                write_file(path, contents, *executable)?;
                rec.changes.push(Change::File {
                    path: path.clone(),
                    hash: hash(contents),
                });
            }
            Op::XmlAction {
                path,
                template,
                id,
                block,
            } => {
                let (text, created_from) = match std::fs::read_to_string(path) {
                    Ok(t) => (t, None),
                    Err(_) => {
                        let t = template
                            .as_ref()
                            .and_then(|t| std::fs::read_to_string(t).ok())
                            .unwrap_or_else(|| crate::linux::EMPTY_UCA.to_owned());
                        let h = hash(t.as_bytes());
                        (t, Some(h))
                    }
                };
                let text = crate::linux::xml_remove(&text, id);
                let text = crate::linux::xml_insert(&text, block)
                    .ok_or_else(|| io::Error::other(format!("{}: no <actions>", path.display())))?;
                make_dirs(path.parent(), rec)?;
                write_file(path, text.as_bytes(), false)?;
                rec.changes.push(Change::XmlAction {
                    path: path.clone(),
                    id: id.clone(),
                    created_from,
                });
            }
            Op::Reg {
                root,
                key,
                name,
                value,
            } => {
                for k in missing_keys(env, *root, key) {
                    rec.changes.push(Change::CreatedKey {
                        root: *root,
                        key: k,
                    });
                }
                let previous = env.registry.get(*root, key, name);
                let value = Value::from(value);
                env.registry.set(*root, key, name, &value)?;
                // A value set twice (the same key in two ops) keeps the first previous.
                rec.changes.push(Change::Reg {
                    root: *root,
                    key: key.clone(),
                    name: name.clone(),
                    value,
                    previous: previous.filter(|_| !owned(rec, *root, key)),
                });
            }
            Op::OwnKey { root, key } => {
                rec.changes.push(Change::OwnKey {
                    root: *root,
                    key: key.clone(),
                });
            }
            Op::Default { types, app } => {
                for ty in types {
                    let previous = env.defaults.current(ty).filter(|p| p != app);
                    env.defaults.set(ty, app).map_err(io::Error::other)?;
                    rec.changes.push(Change::Default {
                        ty: ty.clone(),
                        app: app.clone(),
                        previous,
                    });
                }
            }
            Op::UpdateDesktopDatabase(dir) => {
                env.run_helper("update-desktop-database", &[dir]);
                rec.changes.push(Change::UpdateDesktopDatabase(dir.clone()));
            }
        }
    }
    Ok(())
}

/// Whether `key` lies in a key this record owns (its values are removed with it).
fn owned(rec: &Record, root: Root, key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    rec.changes.iter().any(|c| match c {
        Change::OwnKey { root: r, key: o } => {
            let o = o.to_ascii_lowercase();
            *r == root && (k == o || k.starts_with(&format!("{o}\\")))
        }
        _ => false,
    })
}

/// The keys from the first missing one down to `key`, outermost first.
fn missing_keys(env: &Env, root: Root, key: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut path = String::new();
    for part in key.trim_matches('\\').split('\\') {
        if !path.is_empty() {
            path.push('\\');
        }
        path.push_str(part);
        if !out.is_empty() || !env.registry.key_exists(root, &path) {
            out.push(path.clone());
        }
    }
    out
}

fn make_dirs(dir: Option<&Path>, rec: &mut Record) -> io::Result<()> {
    let Some(dir) = dir else { return Ok(()) };
    let mut missing = Vec::new();
    let mut d = Some(dir);
    while let Some(p) = d {
        if p.exists() {
            break;
        }
        missing.push(p.to_owned());
        d = p.parent();
    }
    std::fs::create_dir_all(dir)?;
    for p in missing.into_iter().rev() {
        rec.changes.push(Change::Dir { path: p });
    }
    Ok(())
}

fn write_file(path: &Path, contents: &[u8], executable: bool) -> io::Result<()> {
    std::fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if executable { 0o755 } else { 0o644 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = executable;
    Ok(())
}

/// Undoes a record, newest change first. Keeps going after errors; returns the first.
pub fn undo(env: &mut Env, rec: &Record) -> io::Result<()> {
    let mut first: Option<io::Error> = None;
    let mut keep = |r: io::Result<()>| {
        if let Err(e) = r
            && first.is_none()
        {
            first = Some(e);
        }
    };
    let mut refresh = Vec::new();
    for c in rec.changes.iter().rev() {
        match c {
            Change::File { path, .. } => keep(remove_file(path)),
            Change::Dir { path } => {
                // Only when empty: anything else in it is not ours.
                let _ = std::fs::remove_dir(path);
            }
            Change::XmlAction {
                path,
                id,
                created_from,
            } => keep(undo_xml(path, id, *created_from)),
            Change::Reg {
                root,
                key,
                name,
                value,
                previous,
            } => {
                // Changed by someone else since: leave it.
                if env.registry.get(*root, key, name).as_ref() == Some(value) {
                    keep(match previous {
                        Some(p) => env.registry.set(*root, key, name, p),
                        None => env.registry.delete_value(*root, key, name),
                    });
                }
            }
            Change::OwnKey { root, key } => keep(env.registry.delete_tree(*root, key)),
            Change::CreatedKey { root, key } => {
                if env.registry.key_exists(*root, key) && env.registry.key_is_empty(*root, key) {
                    keep(env.registry.delete_tree(*root, key));
                }
            }
            Change::Default { ty, app, previous } => {
                if env.defaults.current(ty).as_deref() == Some(app.as_str()) {
                    keep(
                        match previous {
                            Some(p) => env.defaults.set(ty, p),
                            None => env.defaults.unset(ty, app),
                        }
                        .map_err(io::Error::other),
                    );
                }
            }
            Change::UpdateDesktopDatabase(dir) => refresh.push(dir.clone()),
        }
    }
    for dir in refresh {
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        // Only the cache the helper made for our entries is left: it goes too.
        if entries.iter().all(|n| n == "mimeinfo.cache") {
            let _ = std::fs::remove_file(dir.join("mimeinfo.cache"));
        } else {
            env.run_helper("update-desktop-database", &[&dir]);
        }
    }
    first.map_or(Ok(()), Err)
}

/// Removes the empty folders and keys among `list` (innermost first) and keeps the rest,
/// which still hold something of another item's.
pub fn prune(env: &mut Env, list: &mut Vec<Change>) {
    let depth = |c: &Change| match c {
        Change::Dir { path } => path.components().count(),
        Change::CreatedKey { key, .. } => key.split('\\').count(),
        _ => 0,
    };
    list.retain(|c| matches!(c, Change::Dir { .. } | Change::CreatedKey { .. }));
    list.sort_by_key(|c| std::cmp::Reverse(depth(c)));
    list.dedup();
    list.retain(|c| match c {
        Change::Dir { path } => {
            let _ = std::fs::remove_dir(path);
            path.exists()
        }
        Change::CreatedKey { root, key } => {
            if env.registry.key_exists(*root, key) && env.registry.key_is_empty(*root, key) {
                let _ = env.registry.delete_tree(*root, key);
            }
            env.registry.key_exists(*root, key)
        }
        _ => false,
    });
}

fn remove_file(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn undo_xml(path: &Path, id: &str, created_from: Option<u64>) -> io::Result<()> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(());
    };
    let rest = crate::linux::xml_remove(&text, id);
    // A file made for us goes again once it holds only what it started with.
    if let Some(h) = created_from
        && (hash(rest.as_bytes()) == h || !rest.contains("<action>"))
    {
        return remove_file(path);
    }
    if rest != text {
        std::fs::write(path, rest)?;
    }
    Ok(())
}

/// How much of a record is still in place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    All,
    None,
    /// Some is missing: what.
    Some(Vec<String>),
}

pub fn check(env: &Env, rec: &Record) -> Check {
    let mut present = 0;
    let mut missing = Vec::new();
    for c in &rec.changes {
        let ok = match c {
            Change::File { path, .. } => Some((path.is_file(), path.display().to_string())),
            Change::XmlAction { path, id, .. } => Some((
                std::fs::read_to_string(path)
                    .is_ok_and(|t| t.contains(&format!("<unique-id>{id}</unique-id>"))),
                format!("{} in {}", id, path.display()),
            )),
            Change::Reg {
                root,
                key,
                name,
                value,
                ..
            } => Some((
                env.registry.get(*root, key, name).as_ref() == Some(value),
                format!("{}\\{key} [{name}]", root.name()),
            )),
            Change::OwnKey { root, key } => Some((
                env.registry.key_exists(*root, key),
                format!("{}\\{key}", root.name()),
            )),
            Change::Default { ty, app, .. } => Some((
                env.defaults.current(ty).as_deref() == Some(app.as_str()),
                // TRANSLATORS: a missing part of a registration; a file type follows
                pagebara_i18n::tr!("default app for {}", ty),
            )),
            Change::Dir { .. } | Change::CreatedKey { .. } | Change::UpdateDesktopDatabase(_) => {
                None
            }
        };
        match ok {
            Some((true, _)) => present += 1,
            Some((false, what)) => missing.push(what),
            None => {}
        }
    }
    if missing.is_empty() {
        Check::All
    } else if present == 0 {
        Check::None
    } else {
        Check::Some(missing)
    }
}
