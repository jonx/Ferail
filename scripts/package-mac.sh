#!/usr/bin/env bash
#
# package-mac.sh — produce a distributable, signed + notarized + stapled
# Ferail DMG for direct (non-App-Store) macOS distribution.
#
# Pipeline (see docs and Apple's notarization guide):
#   1. Build the release binary and assemble Ferail.app via bundle-mac.sh,
#      signing it with a Developer ID identity under the hardened runtime
#      (HARDENED=1) so Apple will notarize it.
#   2. Pack the .app into a DMG (with an /Applications drop link).
#   3. Sign the DMG, submit it to Apple's notary service, wait for the
#      ticket, and staple it so Gatekeeper validates offline.
#   4. Verify with codesign + spctl.
#
# Prerequisites (one-time, account-level — see ~/Source/apple-codesigning.md):
#   - A "Developer ID Application" certificate in the login keychain.
#   - A stored notary credential profile (xcrun notarytool store-credentials).
#
# Usage:
#   scripts/package-mac.sh                 # full release: sign + notarize + staple
#   scripts/package-mac.sh --no-notarize   # sign + DMG only (offline dry run)
#   FEATURES="--features mpv" scripts/package-mac.sh
#
# Config via env (defaults match the account in apple-codesigning.md):
#   APPLE_DEV_ID        Developer ID Application identity string
#   APPLE_TEAM_ID       Apple Developer team id
#   APPLE_NOTARY_PROFILE  notarytool --keychain-profile name
#
# NOTE: signing hundreds of nested binaries can pop a keychain access
# prompt ("codesign wants to access key ..."). For non-interactive runs,
# pre-authorize the keychain once (needs your login password):
#   security unlock-keychain ~/Library/Keychains/login.keychain-db
#   security set-key-partition-list -S apple-tool:,apple:,codesign: -s \
#     -k "$LOGIN_PW" ~/Library/Keychains/login.keychain-db

set -euo pipefail

cd "$(dirname "$0")/.."
REPO_ROOT="$(pwd)"

APP_NAME="Ferail"
BIN_NAME="ferail-gpui"   # must match bundle-mac.sh
APPLE_DEV_ID="${APPLE_DEV_ID:-Developer ID Application: John Knipper (C43N3NG7Z5)}"
APPLE_TEAM_ID="${APPLE_TEAM_ID:-C43N3NG7Z5}"
APPLE_NOTARY_PROFILE="${APPLE_NOTARY_PROFILE:-D4Mac}"

NOTARIZE=1
if [[ "${1:-}" == "--no-notarize" ]]; then
	NOTARIZE=0
fi

# Read the marketing version from the bundle's Info.plist so the DMG is named
# to match what Finder shows.
VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' \
	"${REPO_ROOT}/packaging/macos/Info.plist" 2>/dev/null || echo 0.0.0)"

DIST_DIR="${REPO_ROOT}/target/dist"
APP_DIR="${REPO_ROOT}/target/${APP_NAME}.app"
DMG_PATH="${DIST_DIR}/${APP_NAME}-${VERSION}.dmg"

# --- 1. Build + assemble + hardened-sign the .app -------------------------
echo "==> [1/4] Building and signing ${APP_NAME}.app with Developer ID"
echo "    identity: ${APPLE_DEV_ID}"
HARDENED=1 CODESIGN_IDENTITY="${APPLE_DEV_ID}" \
	"${REPO_ROOT}/scripts/bundle-mac.sh"

if [[ ! -d "${APP_DIR}" ]]; then
	echo "error: expected ${APP_DIR} after bundle-mac.sh" >&2
	exit 1
fi

mkdir -p "${DIST_DIR}"
xattr -cr "${APP_DIR}"   # strip stray xattrs that would fail codesign/notarize

# --- 1b. Refuse to ship a binary that links a build-machine dylib --------
# A dependency that probes pkg-config (lzma-sys was the first) happily links
# /opt/homebrew/... on a dev Mac. Notarization does NOT catch this: the app
# is perfectly signed and simply dies at launch on a clean machine with
# "Library not loaded". Anything outside /usr/lib and /System is a hard fail.
echo "==> Checking for non-system dylib references"
FOREIGN="$(otool -L "${APP_DIR}/Contents/MacOS/${BIN_NAME}" \
	| tail -n +2 | awk '{print $1}' \
	| grep -vE '^/usr/lib/|^/System/|^@(executable_path|loader_path|rpath)/' || true)"
if [[ -n "${FOREIGN}" ]]; then
	echo "error: the binary links dylibs that will be missing on a clean Mac:" >&2
	echo "${FOREIGN}" | sed 's/^/  /' >&2
	echo "Fix the offending crate (e.g. enable its \"static\"/vendored feature)" >&2
	echo "or bundle + relocate the dylib into Contents/Frameworks." >&2
	exit 1
fi
echo "    ok — only /usr/lib and /System references"

# --- 2. Notarize + staple the .app itself ---------------------------------
# Staple the ticket onto the .app BEFORE packing it, so a user who drags it
# out of the DMG can still first-launch it offline (Gatekeeper reads the
# stapled ticket instead of needing an online notarization check).
if [[ "${NOTARIZE}" == "1" ]]; then
	echo "==> [2/4] Notarizing ${APP_NAME}.app (profile: ${APPLE_NOTARY_PROFILE}) — a few minutes"
	APP_ZIP="${DIST_DIR}/${APP_NAME}-app.zip"
	ditto -c -k --keepParent "${APP_DIR}" "${APP_ZIP}"
	xcrun notarytool submit "${APP_ZIP}" \
		--keychain-profile "${APPLE_NOTARY_PROFILE}" --wait
	rm -f "${APP_ZIP}"
	echo "==> Stapling ticket to ${APP_NAME}.app"
	xcrun stapler staple "${APP_DIR}"
else
	echo "==> [2/4] --no-notarize: skipping app notarization"
fi

# --- 3. Build, sign, notarize + staple the DMG ----------------------------
echo "==> [3/4] Building DMG ${DMG_PATH}"
rm -f "${DMG_PATH}"
STAGE="$(mktemp -d)/dmg"
mkdir -p "${STAGE}"
cp -R "${APP_DIR}" "${STAGE}/${APP_NAME}.app"   # carries the stapled ticket
ln -s /Applications "${STAGE}/Applications"
hdiutil create -volname "${APP_NAME} ${VERSION}" \
	-srcfolder "${STAGE}" -ov -format UDZO "${DMG_PATH}" >/dev/null
rm -rf "$(dirname "${STAGE}")"

echo "==> Signing DMG"
codesign --force --sign "${APPLE_DEV_ID}" --timestamp "${DMG_PATH}"

if [[ "${NOTARIZE}" == "0" ]]; then
	echo "==> [4/4] Signed (not notarized) DMG at ${DMG_PATH}"
	echo "    Gatekeeper will still warn until this is notarized + stapled."
	exit 0
fi

echo "==> Notarizing DMG (profile: ${APPLE_NOTARY_PROFILE})"
xcrun notarytool submit "${DMG_PATH}" \
	--keychain-profile "${APPLE_NOTARY_PROFILE}" --wait
echo "==> Stapling ticket to the DMG"
xcrun stapler staple "${DMG_PATH}"

# --- 4. Verify ------------------------------------------------------------
echo "==> [4/4] Verifying"
xcrun stapler validate "${DMG_PATH}"
spctl -a -t open --context context:primary-signature -vvv "${DMG_PATH}" || true

echo
echo "Done. Distributable DMG (app + DMG both stapled):"
echo "  ${DMG_PATH}"
echo "Verify the app inside with:  spctl -a -vvv /Volumes/.../Ferail.app"
