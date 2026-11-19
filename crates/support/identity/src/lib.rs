//! The application's name and ID. Renaming the app starts here: the packaging scripts
//! (`packaging/identity.sh`) read these constants too, so keep each on one line as
//! `pub const NAME: &str = "...";`.
//!
//! The ID becomes the desktop file / AppStream / Flatpak ID, the macOS bundle identifier and
//! the per-user directory names (except on Linux, which uses `NAME`).

use std::path::PathBuf;

/// The display name (window titles, the about box, package names).
pub const NAME: &str = "Pagebara";

/// Reverse-DNS application ID.
pub const APP_ID: &str = "com.amazynk.Pagebara";

/// The publisher shown by installers.
pub const PUBLISHER: &str = "The Pagebara authors";

/// Short description for package managers and launchers.
pub const SUMMARY: &str = "Fast, private PDF viewer and editor";

/// Project web page.
pub const HOMEPAGE: &str = "https://amazynk.com/pagebara";

/// The source repository: issues, releases and the files packages link to.
pub const REPOSITORY: &str = "https://github.com/amazynk/pagebara";

/// Where the signed update manifest is published (see `crates/update`). Empty: the app
/// never checks for updates and the setting is hidden.
pub const UPDATE_URL: &str = "";

/// The Ed25519 public key (64 hex digits) that signs the update manifest. Empty: updates off.
/// Make the key pair with `packaging/sign-update-manifest --generate-key FILE`.
pub const UPDATE_PUBLIC_KEY: &str = "";

/// The platform's per-user directories for this app.
pub fn project_dirs() -> Option<directories::ProjectDirs> {
    let mut parts = APP_ID.splitn(3, '.');
    let (qualifier, org) = (parts.next()?, parts.next()?);
    directories::ProjectDirs::from(qualifier, org, NAME)
}

/// Per-user configuration directory (settings).
pub fn config_dir() -> Option<PathBuf> {
    project_dirs().map(|d| d.config_dir().to_owned())
}

/// Per-user local data directory (recent files, signatures).
pub fn data_dir() -> Option<PathBuf> {
    project_dirs().map(|d| d.data_local_dir().to_owned())
}

/// Per-user cache directory (library thumbnails, downloaded models).
pub fn cache_dir() -> Option<PathBuf> {
    project_dirs().map(|d| d.cache_dir().to_owned())
}

/// A stable COM class ID for one of the app's Windows shell extensions (`kind`: "thumbnail",
/// "preview", "filter", "persistent-handler"), derived from [`APP_ID`] so a renamed app
/// gets its own. [`guid_string`] formats it.
pub fn windows_clsid(kind: &str) -> u128 {
    // FNV-1a, 128 bits, with the RFC 9562 version 8 (custom) and variant bits.
    let mut h: u128 = 0x6c62272e07bb014262b821756295c58d;
    for b in APP_ID.bytes().chain([0]).chain(kind.bytes()) {
        h ^= u128::from(b);
        h = h.wrapping_mul(0x0000000001000000000000000000013B);
    }
    (h & !(0xf << 76) & !(0x3 << 62)) | (0x8 << 76) | (0x2 << 62)
}

/// `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`, as the registry writes class IDs.
pub fn guid_string(g: u128) -> String {
    let h = format!("{g:032X}");
    format!(
        "{{{}-{}-{}-{}-{}}}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// The Windows shell-extension DLL's file name, next to the executable.
pub const SHELL_DLL: &str = "pagebara_shell_win.dll";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_ids_are_stable_and_distinct() {
        let t = windows_clsid("thumbnail");
        assert_eq!(t, windows_clsid("thumbnail"));
        assert_ne!(t, windows_clsid("preview"));
        let s = guid_string(t);
        assert_eq!(s.len(), 38);
        assert_eq!(&s[15..16], "8");
        assert!(matches!(&s[20..21], "8" | "9" | "A" | "B"), "{s}");
    }

    #[test]
    fn app_id_is_reverse_dns_ending_in_the_name() {
        assert_eq!(APP_ID.split('.').count(), 3);
        assert!(APP_ID.ends_with(NAME));
        assert!(project_dirs().is_some() || std::env::var_os("HOME").is_none());
    }
}
