#!/bin/sh
# Builds <name>-<version>-x64.msi with WiX v4+ (`dotnet tool install --global wix`).
# Runs in Git Bash on Windows (the CI release job).
#
#   packaging/windows/build-msi.sh EXE OUT_DIR [SHELL_DLL]
#
# SHELL_DLL is the Explorer extension (thumbnails, preview pane, search text; built with
# `cargo build --release -p pagebara-shell-win`), by default next to EXE. It is installed next
# to the exe but not registered: the app's Settings > System integration does that per user.
#
# Signing: when WINDOWS_CERT_PFX (base64 .pfx) and WINDOWS_CERT_PASSWORD are set (CI
# secrets), the exe and the MSI are signed with signtool. No certificate exists yet.
set -eu
. "$(dirname "$0")/../identity.sh"
exe=$1
out=$2
dll=${3:-$(dirname "$exe")/$(_identity_const SHELL_DLL)}
[ -f "$dll" ] || { echo "build-msi.sh: $dll not found (cargo build --release -p pagebara-shell-win)" >&2; exit 1; }
mkdir -p "$out"
msi=$out/$APP_NAME-$APP_VERSION-x64.msi
# A stable UpgradeCode for the app ID, so later versions replace earlier ones.
upgrade=$(python -c "import uuid,sys; print(uuid.uuid5(uuid.NAMESPACE_DNS, sys.argv[1]))" "$APP_ID" 2>/dev/null ||
    python3 -c "import uuid,sys; print(uuid.uuid5(uuid.NAMESPACE_DNS, sys.argv[1]))" "$APP_ID")
win() { cygpath -w "$1" 2>/dev/null || echo "$1"; }

sign() {
    [ -n "${WINDOWS_CERT_PFX:-}" ] || return 0
    pfx=$(mktemp)
    echo "$WINDOWS_CERT_PFX" | base64 -d >"$pfx"
    signtool sign /f "$(win "$pfx")" /p "$WINDOWS_CERT_PASSWORD" /fd sha256 \
        /tr http://timestamp.digicert.com /td sha256 "$(win "$1")"
    rm -f "$pfx"
}

sign "$exe"
sign "$dll"
wix build -arch x64 \
    -d Name="$APP_NAME" -d AppId="$APP_ID" -d Publisher="$APP_PUBLISHER" \
    -d Summary="$APP_SUMMARY" -d Homepage="$APP_HOMEPAGE" -d Version="$APP_VERSION" \
    -d UpgradeCode="$upgrade" -d Exe="$(win "$exe")" -d ExeName="$(basename "$exe")" \
    -d ShellDll="$(win "$dll")" -d ShellDllName="$(basename "$dll")" \
    -d Icon="$(win "$ROOT/packaging/icons/app.ico")" -d Root="$(win "$ROOT")" \
    -o "$(win "$msi")" "$(win "$ROOT/packaging/windows/app.wxs")"
sign "$msi"
echo "$msi"
