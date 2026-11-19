//! Desktop integration that needs no UI: forwarding files to the running instance,
//! reading `file://` URIs and `file.pdf#page=N` arguments, reporting opened files to the
//! system's recent list (freedesktop `recently-used.xbel`) and default-app registration.
//!
//! The optional, per-item integration (thumbnails, menus, the default app and more) is in
//! `pagebara-integration`; the native calls it needs (macOS Apple events, Windows jump lists)
//! in `pagebara-native`.
//!
//! Not done:
//! - Wayland: raising the window for a second launch needs the launcher's activation token
//!   applied to our surface, which winit 0.30 has no call for; the window asks for attention.

pub mod default_app;
pub mod instance;
pub mod recent_xbel;
pub mod request;
pub mod uri;

pub use instance::{Instance, InstanceKey, Server};
pub use request::{OpenFile, OpenRequest};
