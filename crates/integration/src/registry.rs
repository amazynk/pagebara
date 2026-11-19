//! The Windows registry as the integration code sees it: the real one on Windows, or a
//! JSON file standing in for it (`--profile` runs and tests, on any platform).

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Root {
    /// HKEY_CURRENT_USER: everything per user.
    CurrentUser,
    /// HKEY_LOCAL_MACHINE: only the Windows Search filter (the indexer reads no user keys).
    LocalMachine,
}

impl Root {
    pub fn name(self) -> &'static str {
        match self {
            Root::CurrentUser => "HKCU",
            Root::LocalMachine => "HKLM",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Value {
    String(String),
    Dword(u32),
}

/// Registry access. Key paths use `\`; the value name `""` is the key's default value.
pub trait Registry: Send {
    fn key_exists(&self, root: Root, key: &str) -> bool;
    /// Whether the key has neither subkeys nor values.
    fn key_is_empty(&self, root: Root, key: &str) -> bool;
    fn get(&self, root: Root, key: &str, name: &str) -> Option<Value>;
    /// Sets a value, creating the key and its parents.
    fn set(&mut self, root: Root, key: &str, name: &str, value: &Value) -> io::Result<()>;
    fn delete_value(&mut self, root: Root, key: &str, name: &str) -> io::Result<()>;
    /// Deletes a key with everything below it; a missing key is not an error.
    fn delete_tree(&mut self, root: Root, key: &str) -> io::Result<()>;
}

/// Keys compare without regard to case, as Windows does.
fn norm(key: &str) -> String {
    key.trim_matches('\\').to_ascii_lowercase()
}

/// An in-memory registry, saved to a JSON file when it has one.
#[derive(Debug, Default)]
pub struct FileRegistry {
    path: Option<PathBuf>,
    /// (root, lower-case key) -> (key as written, values).
    keys: BTreeMap<(Root, String), (String, BTreeMap<String, Value>)>,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    keys: Vec<(Root, String, BTreeMap<String, Value>)>,
}

impl FileRegistry {
    /// Memory only (tests).
    pub fn new() -> FileRegistry {
        FileRegistry::default()
    }

    /// Kept in `path` (loaded now, saved after each change).
    pub fn open(path: PathBuf) -> FileRegistry {
        let mut r = FileRegistry {
            path: Some(path.clone()),
            keys: BTreeMap::new(),
        };
        if let Some(saved) = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Saved>(&b).ok())
        {
            for (root, key, values) in saved.keys {
                r.keys.insert((root, norm(&key)), (key, values));
            }
        }
        r
    }

    fn save(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let saved = Saved {
            keys: self
                .keys
                .iter()
                .map(|((root, _), (key, v))| (*root, key.clone(), v.clone()))
                .collect(),
        };
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(&saved)?)
    }

    /// Every key, as `HKCU\key` (tests and `--integration status --verbose`).
    pub fn dump(&self) -> Vec<String> {
        self.keys
            .iter()
            .flat_map(|((root, _), (key, values))| {
                let k = format!("{}\\{key}", root.name());
                if values.is_empty() {
                    vec![k]
                } else {
                    values
                        .iter()
                        .map(|(n, v)| format!("{k} [{n}] = {v:?}"))
                        .collect()
                }
            })
            .collect()
    }
}

impl Registry for FileRegistry {
    fn key_exists(&self, root: Root, key: &str) -> bool {
        self.keys.contains_key(&(root, norm(key)))
    }

    fn key_is_empty(&self, root: Root, key: &str) -> bool {
        let k = norm(key);
        let prefix = format!("{k}\\");
        match self.keys.get(&(root, k)) {
            None => true,
            Some((_, values)) => {
                values.is_empty()
                    && !self
                        .keys
                        .keys()
                        .any(|(r, other)| *r == root && other.starts_with(&prefix))
            }
        }
    }

    fn get(&self, root: Root, key: &str, name: &str) -> Option<Value> {
        let (_, values) = self.keys.get(&(root, norm(key)))?;
        values
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    }

    fn set(&mut self, root: Root, key: &str, name: &str, value: &Value) -> io::Result<()> {
        let key = key.trim_matches('\\');
        // Parents exist too, as in the real registry.
        let mut path = String::new();
        for part in key.split('\\') {
            if !path.is_empty() {
                path.push('\\');
            }
            path.push_str(part);
            self.keys
                .entry((root, norm(&path)))
                .or_insert_with(|| (path.clone(), BTreeMap::new()));
        }
        let (_, values) = self.keys.get_mut(&(root, norm(key))).expect("just made");
        values.retain(|n, _| !n.eq_ignore_ascii_case(name));
        values.insert(name.to_owned(), value.clone());
        self.save()
    }

    fn delete_value(&mut self, root: Root, key: &str, name: &str) -> io::Result<()> {
        if let Some((_, values)) = self.keys.get_mut(&(root, norm(key))) {
            values.retain(|n, _| !n.eq_ignore_ascii_case(name));
        }
        self.save()
    }

    fn delete_tree(&mut self, root: Root, key: &str) -> io::Result<()> {
        let k = norm(key);
        let prefix = format!("{k}\\");
        self.keys
            .retain(|(r, other), _| !(*r == root && (*other == k || other.starts_with(&prefix))));
        self.save()
    }
}

/// The real registry.
#[cfg(windows)]
#[derive(Debug, Default)]
pub struct WindowsRegistry;

#[cfg(windows)]
impl WindowsRegistry {
    fn root(root: Root) -> &'static windows_registry::Key {
        match root {
            Root::CurrentUser => windows_registry::CURRENT_USER,
            Root::LocalMachine => windows_registry::LOCAL_MACHINE,
        }
    }
}

#[cfg(windows)]
fn win<T>(r: windows_registry::Result<T>) -> io::Result<T> {
    r.map_err(|e| {
        // ERROR_ACCESS_DENIED (HKLM without elevation).
        if e.code().0 as u32 == 0x8007_0005 {
            io::Error::new(io::ErrorKind::PermissionDenied, e.message())
        } else {
            io::Error::other(e.message())
        }
    })
}

#[cfg(windows)]
impl Registry for WindowsRegistry {
    fn key_exists(&self, root: Root, key: &str) -> bool {
        Self::root(root).open(key).is_ok()
    }

    fn key_is_empty(&self, root: Root, key: &str) -> bool {
        match Self::root(root).open(key) {
            Err(_) => true,
            Ok(k) => {
                k.keys().map(|mut i| i.next().is_none()).unwrap_or(true)
                    && k.values().map(|mut i| i.next().is_none()).unwrap_or(true)
            }
        }
    }

    fn get(&self, root: Root, key: &str, name: &str) -> Option<Value> {
        let k = Self::root(root).open(key).ok()?;
        if let Ok(s) = k.get_string(name) {
            return Some(Value::String(s));
        }
        k.get_u32(name).ok().map(Value::Dword)
    }

    fn set(&mut self, root: Root, key: &str, name: &str, value: &Value) -> io::Result<()> {
        let k = win(Self::root(root).create(key))?;
        win(match value {
            Value::String(s) => k.set_string(name, s),
            Value::Dword(d) => k.set_u32(name, *d),
        })
    }

    fn delete_value(&mut self, root: Root, key: &str, name: &str) -> io::Result<()> {
        match Self::root(root).options().write().open(key) {
            Ok(k) => match k.remove_value(name) {
                Ok(()) => Ok(()),
                Err(_) if k.get_value(name).is_err() => Ok(()),
                Err(e) => win(Err(e)),
            },
            Err(_) => Ok(()),
        }
    }

    fn delete_tree(&mut self, root: Root, key: &str) -> io::Result<()> {
        if !self.key_exists(root, key) {
            return Ok(());
        }
        win(Self::root(root).remove_tree(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_registry_keys_values_and_trees() {
        let mut r = FileRegistry::new();
        let v = Value::String("x".into());
        r.set(Root::CurrentUser, r"Software\Classes\A\B", "", &v)
            .unwrap();
        assert!(r.key_exists(Root::CurrentUser, r"software\classes\a"));
        assert!(!r.key_is_empty(Root::CurrentUser, r"Software\Classes\A"));
        assert_eq!(
            r.get(Root::CurrentUser, r"SOFTWARE\Classes\A\B", ""),
            Some(v)
        );
        assert!(!r.key_exists(Root::LocalMachine, r"Software\Classes\A"));
        r.delete_value(Root::CurrentUser, r"Software\Classes\A\B", "")
            .unwrap();
        assert!(r.key_is_empty(Root::CurrentUser, r"Software\Classes\A\B"));
        r.delete_tree(Root::CurrentUser, r"Software\Classes\A")
            .unwrap();
        assert!(!r.key_exists(Root::CurrentUser, r"Software\Classes\A\B"));
        assert!(r.key_exists(Root::CurrentUser, r"Software\Classes"));
    }

    #[test]
    fn file_registry_persists() {
        let p = std::env::temp_dir().join(format!("pagebara-reg-{}.json", std::process::id()));
        let mut r = FileRegistry::open(p.clone());
        r.set(Root::CurrentUser, r"K\L", "n", &Value::Dword(3))
            .unwrap();
        let r = FileRegistry::open(p.clone());
        assert_eq!(r.get(Root::CurrentUser, r"k\l", "N"), Some(Value::Dword(3)));
        std::fs::remove_file(p).unwrap();
    }
}
