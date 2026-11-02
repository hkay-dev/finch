#!/usr/bin/env bash
# Builds Finch for release and makes target/release/Finch.app: the Info.plist, the app icon
# (compiled from AppIcon.icon with actool), the user guide, and a signature (scripts/sign-identity.sh
# picks the identity; ad hoc when none is set). Prints the app's path on stdout, and everything else
# on stderr. Needs the Rust toolchain and Xcode's command line tools, nothing else.
#
#   scripts/bundle.sh
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
root=$PWD
source "$root/scripts/sign-identity.sh"
exec 3>&1 1>&2 # stdout is only the app's path

name="Finch"
exe="finch"
target="${CARGO_TARGET_DIR:-$root/target}"
app="$target/release/$name.app"
version="$(git rev-parse --short=9 HEAD 2>/dev/null || echo 0)"

echo "bundle: building $name with cargo build --release"
cargo build --release
[[ -x "$target/release/$exe" ]] || { echo "bundle: cargo build made no $exe" >&2; exit 1; }

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$target/release/$exe" "$app/Contents/MacOS/$exe"

# The icon: AppIcon.icon compiled into Resources (Assets.car and AppIcon.icns).
icon_keys=""
if [[ -d "$root/AppIcon.icon" ]]; then
    scratch="$(mktemp -d)"
    trap 'rm -rf "${scratch:?}"' EXIT
    xcrun actool "$root/AppIcon.icon" --compile "$app/Contents/Resources" --app-icon AppIcon --platform macosx \
        --minimum-deployment-target 14.0 --output-partial-info-plist "$scratch/icon.plist" >/dev/null ||
        { echo "bundle: actool failed to compile AppIcon.icon" >&2; exit 1; }
    icon_keys="    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>CFBundleIconName</key><string>AppIcon</string>"
fi

# The app's own plist keys: retina, malloc's space-efficient mode (src/space_efficient.rs), the
# Documents prompt's reason (the Scrap Drafts folder), the folder and text document types it
# opens, and the two Services menu items (src/services.rs: the provider's methods are the
# NSMessage names; NSPortName is the bundle name).
cat >"$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key><string>$exe</string>
    <key>CFBundleIdentifier</key><string>dev.hkay.finch</string>
    <key>CFBundleName</key><string>$name</string>
    <key>CFBundlePackageType</key><string>APPL</string>
$icon_keys
    <key>CFBundleShortVersionString</key><string>1.0</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSMinimumSystemVersion</key><string>14.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSHumanReadableCopyright</key><string>© 2026 Harsha Kotcherlakota</string>
    <key>NSDocumentsFolderUsageDescription</key><string>Finch keeps your Untitled pages in Documents so you never lose them.</string>
    <key>LSEnvironment</key>
    <dict><key>MallocSpaceEfficient</key><string>1</string></dict>
    <key>CFBundleDocumentTypes</key>
    <array>
        <dict>
            <key>CFBundleTypeName</key><string>Folder</string>
            <key>CFBundleTypeRole</key><string>Editor</string>
            <key>LSHandlerRank</key><string>Alternate</string>
            <key>LSItemContentTypes</key>
            <array><string>public.folder</string></array>
        </dict>
        <dict>
            <key>CFBundleTypeName</key><string>Text document</string>
            <key>CFBundleTypeRole</key><string>Editor</string>
            <key>LSHandlerRank</key><string>Alternate</string>
            <key>LSItemContentTypes</key>
            <array>
                <string>public.text</string>
                <string>public.plain-text</string>
                <string>public.source-code</string>
                <string>public.data</string>
            </array>
        </dict>
    </array>
    <key>NSServices</key>
    <array>
        <dict>
            <key>NSMenuItem</key>
            <dict><key>default</key><string>Append to Finch Note</string></dict>
            <key>NSMessage</key><string>appendToFinchNote</string>
            <key>NSPortName</key><string>Finch</string>
            <key>NSSendTypes</key>
            <array><string>NSStringPboardType</string><string>public.utf8-plain-text</string></array>
        </dict>
        <dict>
            <key>NSMenuItem</key>
            <dict><key>default</key><string>New Finch Note</string></dict>
            <key>NSMessage</key><string>newFinchNote</string>
            <key>NSPortName</key><string>Finch</string>
            <key>NSSendTypes</key>
            <array><string>NSStringPboardType</string><string>public.utf8-plain-text</string></array>
        </dict>
    </array>
</dict>
</plist>
PLIST

# Help > Finch Help opens Contents/Resources/guide/index.md (src/app.rs `guide_dir`). The pages link
# images as ../assets/<name>, so the images they name go to Resources/assets.
mkdir -p "$app/Contents/Resources/guide" "$app/Contents/Resources/assets"
cp "$root"/docs/guide/*.md "$app/Contents/Resources/guide/"
{ grep -ho '(\.\./assets/[^)]*)' "$root"/docs/guide/*.md || true; } | sed 's/^(\.\.\/assets\///; s/)$//' | sort -u |
    while read -r image; do cp "$root/docs/assets/$image" "$app/Contents/Resources/assets/"; done

echo "bundle: signing with $SIGN_IDENTITY"
codesign --force --deep -s "$SIGN_IDENTITY" "$app"
codesign --verify --deep --strict "$app"

echo "$app" >&3
