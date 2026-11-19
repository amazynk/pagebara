//! External links.

/// The publisher’s website and the stable redirect to its community invite.
pub const WEBSITE_URL: &str = "https://amazynk.com/";
pub const DISCORD_INVITE_URL: &str = "https://amazynk.com/discord/";

pub fn open(url: &str) {
    if url.is_empty() {
        return;
    }
    if let Err(e) = ::open::that_detached(url) {
        log::warn!("could not open {url}: {e}");
    }
}
