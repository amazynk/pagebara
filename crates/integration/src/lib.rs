//! Optional desktop integration, item by item. Each item (see [`catalog`]) is either a
//! registration with the system, an app setting, or a button to the system's own page.
//! Registrations are planned as a list of steps ([`plan::Op`]: files, registry values,
//! default-app choices), carried out with a record of each change in a manifest, and
//! turned off by undoing exactly those changes: files and keys we made are removed,
//! values we replaced get their earlier value back, and anything changed by someone
//! else since is left alone.
//!
//! Everything is per user (no administrator rights), except the Windows Search filter,
//! which Windows reads from machine-wide keys only. Installers register only the basics
//! (the launcher entry, "Open with"); the rest is the user's choice in the app.
//!
//! Not built, with the reasons, in [`catalog::SKIPPED`] and [`catalog::unavailable`].

pub mod catalog;
pub mod env;
mod linux;
mod lnk;
mod macos;
pub mod plan;
pub mod registry;
mod windows;

pub use catalog::{Group, Item, Kind, MENU_ACTIONS, MenuAction};
pub use env::{DefaultApps, Env, NoDefaults, XdgDefaults};
pub use linux::{FDF_TYPES, IMAGE_TYPES, PDF_TYPES};

use plan::{Check, Manifest, Op, Record};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Os {
    Linux,
    Windows,
    MacOs,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else {
            Os::Linux
        }
    }
}

/// Why an item has nothing to register here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoPlan {
    /// It cannot work on this system: why.
    Unavailable(String),
    /// The package or bundle already does it: how.
    Provided(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    On,
    Off,
    /// Turned on, but some of it is gone: what.
    Partial(Vec<String>),
    /// Turned on for an earlier location of the app; turning it on again repairs it.
    Outdated,
    Unavailable(String),
    Provided(String),
    /// An app setting: the app knows.
    Setting,
    /// Opens the system's page.
    Button,
}

impl Status {
    pub fn is_on(&self) -> bool {
        matches!(
            self,
            Status::On | Status::Partial(_) | Status::Outdated | Status::Provided(_)
        )
    }

    /// Short text for the settings page and `--integration status`.
    pub fn text(&self) -> String {
        match self {
            // Not translated: the apps compare these two.
            Status::On => "On".into(),
            Status::Off => "Off".into(),
            Status::Partial(missing) => {
                pagebara_i18n::tr!("Partly removed: {}", missing.join(", "))
            }
            Status::Outdated => {
                pagebara_i18n::tr!("Set up for another copy of the app; turn on again to repair")
            }
            Status::Unavailable(why) => why.clone(),
            Status::Provided(how) => how.clone(),
            Status::Setting | Status::Button => String::new(),
        }
    }
}

#[derive(Debug)]
pub enum Error {
    UnknownItem(String),
    Unavailable(String),
    /// Machine-wide keys need administrator rights (the Windows Search filter).
    NeedsAdmin,
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::UnknownItem(id) => write!(f, "unknown item {id}"),
            Error::Unavailable(why) => f.write_str(why),
            Error::NeedsAdmin => f.write_str(&pagebara_i18n::tr!("needs administrator rights")),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            Error::NeedsAdmin
        } else {
            Error::Io(e)
        }
    }
}

/// What a change asks of the caller.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Items changed (the one asked for and those that depend on it or it on them).
    pub changed: Vec<String>,
    /// Refresh Explorer associations or Finder Quick Actions.
    pub shell_changed: bool,
}

/// The items `id` needs on `os` (turned on with it).
fn requires(env: &Env, id: &str) -> Vec<&'static str> {
    match (env.os, id) {
        (Os::Linux, "default-pdf") if !linux::launcher_known(env) => vec!["open-with"],
        (Os::Linux, "default-fdf") => vec!["type-fdf"],
        (Os::Windows, "type-fdf") => vec!["open-with"],
        _ => Vec::new(),
    }
}

/// Integration for one user (or one sandbox).
#[derive(Debug)]
pub struct Integration {
    pub env: Env,
    manifest: Manifest,
}

impl Integration {
    pub fn new(env: Env) -> Integration {
        let manifest = Manifest::load(&env.manifest);
        Integration { env, manifest }
    }

    pub fn items(&self) -> Vec<Item> {
        catalog::items(self.env.os)
    }

    fn item(&self, id: &str) -> Result<Item, Error> {
        catalog::item(self.env.os, id).ok_or_else(|| Error::UnknownItem(id.to_owned()))
    }

    pub fn plan(&self, id: &str) -> Result<Vec<Op>, NoPlan> {
        match self.env.os {
            Os::Linux => linux::plan(&self.env, id),
            Os::Windows => windows::plan(&self.env, id),
            Os::MacOs => macos::plan(&self.env, id),
        }
    }

    /// The items registered now (per the manifest).
    pub fn registered(&self) -> Vec<String> {
        self.manifest.items.keys().cloned().collect()
    }

    pub fn status(&self, id: &str) -> Status {
        let Ok(item) = self.item(id) else {
            return Status::Unavailable("unknown item".into());
        };
        if let Some(why) = catalog::unavailable(self.env.os, id) {
            return Status::Unavailable(pagebara_i18n::translate(why, &[]));
        }
        match item.kind {
            Kind::Setting => return Status::Setting,
            Kind::Button => return Status::Button,
            Kind::Registration => {}
        }
        let plan = self.plan(id);
        let Some(rec) = self.manifest.items.get(id) else {
            return match plan {
                Err(NoPlan::Unavailable(w)) => Status::Unavailable(w),
                Err(NoPlan::Provided(w)) => Status::Provided(w),
                // Chosen outside the app (the default app in system settings).
                Ok(ops) if self.defaults_already(&ops) => Status::On,
                Ok(_) => Status::Off,
            };
        };
        match plan::check(&self.env, rec) {
            Check::None => Status::Off,
            Check::Some(missing) => Status::Partial(missing),
            Check::All => match plan {
                Ok(ops) if plan::fingerprint(&ops) != rec.fingerprint => Status::Outdated,
                _ => Status::On,
            },
        }
    }

    fn defaults_already(&self, ops: &[Op]) -> bool {
        let mut any = false;
        for op in ops {
            match op {
                Op::Default { types, app } => {
                    any = true;
                    if !types
                        .iter()
                        .all(|t| self.env.defaults.current(t).as_deref() == Some(app.as_str()))
                    {
                        return false;
                    }
                }
                _ => return false,
            }
        }
        any
    }

    /// Turns `id` on (again: a second time repairs it), with the items it needs.
    pub fn enable(&mut self, id: &str) -> Result<Outcome, Error> {
        let item = self.item(id)?;
        if item.kind != Kind::Registration {
            return Err(Error::Unavailable(format!("{id} is not a registration")));
        }
        if let Some(why) = catalog::unavailable(self.env.os, id) {
            return Err(Error::Unavailable(pagebara_i18n::translate(why, &[])));
        }
        let mut out = Outcome::default();
        for dep in requires(&self.env, id) {
            if !self.status(dep).is_on() {
                let o = self.enable(dep)?;
                out.changed.extend(o.changed);
                out.shell_changed |= o.shell_changed;
            }
        }
        let ops = match self.plan(id) {
            Ok(ops) => ops,
            Err(NoPlan::Unavailable(w)) => return Err(Error::Unavailable(w)),
            Err(NoPlan::Provided(_)) => return Ok(out),
        };
        if let Some(old) = self.manifest.items.remove(id) {
            plan::undo(&mut self.env, &old)?;
            self.forget(old);
        }
        let mut rec = Record::default();
        if let Err(e) = plan::apply(&mut self.env, &ops, &mut rec) {
            // Leave nothing half done.
            let _ = plan::undo(&mut self.env, &rec);
            self.forget(rec);
            self.save()?;
            return Err(e.into());
        }
        self.manifest.items.insert(id.to_owned(), rec);
        self.save()?;
        out.changed.push(id.to_owned());
        out.shell_changed |= self.env.os == Os::Windows || item.group == Group::Finder;
        Ok(out)
    }

    /// Turns `id` off: undoes what was recorded for it (and for items that need it).
    pub fn disable(&mut self, id: &str) -> Result<Outcome, Error> {
        let item = self.item(id)?;
        let mut out = Outcome::default();
        let dependents: Vec<String> = self
            .manifest
            .items
            .keys()
            .filter(|k| requires(&self.env, k).contains(&id))
            .cloned()
            .collect();
        for d in dependents {
            let o = self.disable(&d)?;
            out.shell_changed |= o.shell_changed;
            out.changed.extend(o.changed);
        }
        if let Some(rec) = self.manifest.items.remove(id) {
            let r = plan::undo(&mut self.env, &rec);
            if let Err(e) = r {
                // Keep the record: turning it off again retries.
                self.manifest.items.insert(id.to_owned(), rec);
                self.save()?;
                return Err(e.into());
            }
            self.forget(rec);
            self.save()?;
            out.changed.push(id.to_owned());
            out.shell_changed |= self.env.os == Os::Windows || item.group == Group::Finder;
        }
        Ok(out)
    }

    /// Undoes every registration. Returns the first error after trying them all.
    pub fn remove_all(&mut self) -> Result<Outcome, Error> {
        let mut out = Outcome::default();
        let mut first = None;
        for id in self.registered() {
            match self.disable(&id) {
                Ok(o) => {
                    out.changed.extend(o.changed);
                    out.shell_changed |= o.shell_changed;
                }
                Err(e) => {
                    first.get_or_insert(e);
                }
            }
        }
        first.map_or(Ok(out), Err)
    }

    /// Keeps the folders and keys of an undone record that other items still use, and
    /// removes those that are empty now.
    fn forget(&mut self, rec: Record) {
        let mut list = std::mem::take(&mut self.manifest.leftovers);
        list.extend(rec.changes);
        plan::prune(&mut self.env, &mut list);
        self.manifest.leftovers = list;
    }

    fn save(&self) -> Result<(), Error> {
        Ok(self.manifest.save(&self.env.manifest)?)
    }
}
