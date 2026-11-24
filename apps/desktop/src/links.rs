//! External links.

/// The publisher’s website and the stable redirect to its community invite.
pub const WEBSITE_URL: &str = "https://amazynk.com/";
pub const DISCORD_INVITE_URL: &str = "https://amazynk.com/discord/";

/// Whether About names Dozed: it runs on Linux only, so it is offered there only.
pub const DOZED_OFFERED: bool = cfg!(target_os = "linux");

/// The page of another Amazynk app, where its downloads are. The browser fetches it;
/// Pagebara downloads nothing itself. `None` for a name that is not one of the apps.
pub fn app_page(app: &str) -> Option<&'static str> {
    match app {
        "specular" => Some("https://amazynk.com/specular/"),
        "dozed" => Some("https://amazynk.com/dozed/"),
        // Zynk has a site of its own.
        "zynk" => Some("https://zynk.it/"),
        _ => None,
    }
}

pub fn open(url: &str) {
    if url.is_empty() {
        return;
    }
    if let Err(e) = ::open::that_detached(url) {
        log::warn!("could not open {url}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The About page's rows open exactly these pages: plain https addresses on the
    /// publisher's site, with no query or fragment that could say who clicked.
    #[test]
    fn app_pages_are_plain_links_to_the_publishers_site() {
        for app in ["specular", "dozed"] {
            let url = app_page(app).unwrap();
            assert_eq!(url, format!("{WEBSITE_URL}{app}/"));
            assert!(!url.contains(['?', '#']), "{url}");
        }
        let zynk = app_page("zynk").unwrap();
        assert_eq!(zynk, "https://zynk.it/");
        assert!(!zynk.contains(['?', '#']), "{zynk}");
        assert_eq!(app_page("pagebara"), None);
        assert_eq!(app_page(""), None);
    }
}
