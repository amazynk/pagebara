# Sourced by the packaging scripts: the app's name, ID and version, read from their one
# source (crates/support/identity/src/lib.rs and the workspace Cargo.toml).
# Sets ROOT, APP_NAME, APP_ID, APP_PUBLISHER, APP_SUMMARY, APP_HOMEPAGE, APP_REPOSITORY, APP_VERSION,
# APP_BIN,
# APP_UPDATE_URL and APP_UPDATE_KEY (both empty until updates are published).
# The sourcing script lives in packaging/<platform>/.
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
_identity_const() {
    sed -n "s/^pub const $1: &str = \"\(.*\)\";/\1/p" "$ROOT/crates/support/identity/src/lib.rs"
}
APP_NAME=$(_identity_const NAME)
APP_ID=$(_identity_const APP_ID)
APP_PUBLISHER=$(_identity_const PUBLISHER)
APP_SUMMARY=$(_identity_const SUMMARY)
APP_HOMEPAGE=$(_identity_const HOMEPAGE)
APP_REPOSITORY=$(_identity_const REPOSITORY)
APP_UPDATE_URL=$(_identity_const UPDATE_URL)
APP_UPDATE_KEY=$(_identity_const UPDATE_PUBLIC_KEY)
APP_VERSION=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml")
# The executable's name, from [[bin]] in apps/desktop/Cargo.toml.
APP_BIN=$(sed -n '/^\[\[bin\]\]/,/^\[/s/^name = "\(.*\)"/\1/p' "$ROOT/apps/desktop/Cargo.toml")
: "${APP_NAME:?} ${APP_ID:?} ${APP_VERSION:?} ${APP_BIN:?}"

# Replaces @APP_NAME@, @APP_ID@, ... in a template file ($1) and writes it to $2.
fill_template() {
    sed -e "s|@APP_NAME@|$APP_NAME|g" -e "s|@APP_ID@|$APP_ID|g" \
        -e "s|@APP_PUBLISHER@|$APP_PUBLISHER|g" -e "s|@APP_SUMMARY@|$APP_SUMMARY|g" \
        -e "s|@APP_HOMEPAGE@|$APP_HOMEPAGE|g" -e "s|@APP_REPOSITORY@|$APP_REPOSITORY|g" \
        -e "s|@APP_VERSION@|$APP_VERSION|g" \
        -e "s|@APP_BIN@|$APP_BIN|g" -e "s|@APP_DATE@|${APP_DATE:-$(date -u +%Y-%m-%d)}|g" \
        "$1" >"$2"
}
