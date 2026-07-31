#!/usr/bin/env bash
#
# bundle-mac.sh — assemble a Ferail.app bundle so macOS treats Ferail
# as a real signed app and shows the automatic TCC consent prompts
# ("Ferail would like to access files in your Documents folder") instead
# of just failing with EPERM.
#
# Why this is needed: the consent prompt only appears for a code-signed
# .app bundle whose Info.plist declares the matching NS*UsageDescription
# strings (see packaging/macos/Info.plist). Running the loose binary via
# `cargo run` from a terminal can't prompt — it inherits the terminal's
# privacy identity and has no usage strings to show.
#
# Usage:
#   scripts/bundle-mac.sh            # release build, ad-hoc signature
#   PROFILE=debug scripts/bundle-mac.sh
#   CODESIGN_IDENTITY="Developer ID Application: …" scripts/bundle-mac.sh
#
# Ad-hoc signing (the default, "-") is enough to make the prompt APPEAR,
# but its identity (a content hash) changes on every rebuild, so macOS
# treats each build as a new app and re-prompts / forgets prior grants.
# For grants that persist across rebuilds, sign with a stable Developer
# ID via CODESIGN_IDENTITY.

set -euo pipefail

cd "$(dirname "$0")/.."
REPO_ROOT="$(pwd)"

PROFILE="${PROFILE:-release}"
BIN_NAME="ferail-gpui"
APP_NAME="Ferail"
IDENTITY="${CODESIGN_IDENTITY:--}" # "-" == ad-hoc

# Pass extra cargo flags through, e.g. FEATURES="--features mpv".
FEATURES="${FEATURES:-}"

echo "==> Building ${BIN_NAME} (${PROFILE})"
if [[ "${PROFILE}" == "release" ]]; then
	cargo build --release --bin "${BIN_NAME}" ${FEATURES}
	BIN_PATH="${REPO_ROOT}/target/release/${BIN_NAME}"
else
	cargo build --bin "${BIN_NAME}" ${FEATURES}
	BIN_PATH="${REPO_ROOT}/target/debug/${BIN_NAME}"
fi

if [[ ! -x "${BIN_PATH}" ]]; then
	echo "error: built binary not found at ${BIN_PATH}" >&2
	exit 1
fi

APP_DIR="${REPO_ROOT}/target/${APP_NAME}.app"
CONTENTS="${APP_DIR}/Contents"
MACOS_DIR="${CONTENTS}/MacOS"
RES_DIR="${CONTENTS}/Resources"

echo "==> Assembling ${APP_DIR}"
rm -rf "${APP_DIR}"
mkdir -p "${MACOS_DIR}" "${RES_DIR}"

cp "${BIN_PATH}" "${MACOS_DIR}/${BIN_NAME}"
cp "${REPO_ROOT}/packaging/macos/Info.plist" "${CONTENTS}/Info.plist"

# Licenses travel with the binary. MIT/Apache-2.0 (and the MIT tree-sitter
# grammars, the ISC/MIT icon artwork) require their notices to accompany a
# redistributed copy — a DMG containing only the executable does not
# satisfy that. Ship them inside the bundle so every copy carries them.
LIC_DIR="${RES_DIR}/licenses"
mkdir -p "${LIC_DIR}"
for f in LICENSE-MIT LICENSE-APACHE THIRD-PARTY-NOTICES.md; do
	if [[ -f "${REPO_ROOT}/${f}" ]]; then
		cp "${REPO_ROOT}/${f}" "${LIC_DIR}/${f}"
	else
		echo "warning: ${f} missing — bundle will under-attribute" >&2
	fi
done
echo "==> Copied Resources/licenses ($(ls -1 "${LIC_DIR}" | wc -l | tr -d ' ') files)"

# Icon: prefer the checked-in macOS .icns so bundle output stays stable.
# If it is missing, regenerate from the macOS PNG source as a best-effort
# fallback. Windows keeps using resources/ferail.ico via build.rs.
ICNS_SRC="${REPO_ROOT}/crates/ferail-gpui/resources/ferail-macos.icns"
PNG_SRC="${REPO_ROOT}/crates/ferail-gpui/resources/ferail-macos.png"
if [[ -f "${ICNS_SRC}" ]]; then
	cp "${ICNS_SRC}" "${RES_DIR}/ferail.icns"
	echo "==> Copied Resources/ferail.icns"
elif [[ -f "${PNG_SRC}" ]] && command -v iconutil >/dev/null 2>&1; then
	ICONSET="$(mktemp -d)/ferail.iconset"
	mkdir -p "${ICONSET}"
	for size in 16 32 64 128 256 512; do
		sips -z "${size}" "${size}" "${PNG_SRC}" \
			--out "${ICONSET}/icon_${size}x${size}.png" >/dev/null 2>&1 || true
		dbl=$((size * 2))
		sips -z "${dbl}" "${dbl}" "${PNG_SRC}" \
			--out "${ICONSET}/icon_${size}x${size}@2x.png" >/dev/null 2>&1 || true
	done
	if iconutil -c icns "${ICONSET}" -o "${RES_DIR}/ferail.icns" 2>/dev/null; then
		echo "==> Wrote Resources/ferail.icns"
	else
		echo "warning: iconutil failed; bundle has no icon" >&2
	fi
	rm -rf "$(dirname "${ICONSET}")"
else
	echo "warning: no macOS icon source or iconutil; bundle has no icon" >&2
fi

# Register the bundle with the Info.plist version so Finder shows metadata.
/usr/bin/plutil -lint "${CONTENTS}/Info.plist" >/dev/null

echo "==> Signing (identity: ${IDENTITY})"
# Two signing modes:
#   HARDENED=1 -> release signing for notarization: hardened runtime
#     (--options runtime), a secure timestamp, and the entitlements that
#     let an mpv build dlopen an unsigned libmpv. Used by package-mac.sh.
#   default    -> ad-hoc / quick dev signing. No hardened runtime: TCC
#     consent prompts don't need it, and re-signs stay fast. (This is NOT
#     the sandbox either way — a file manager needs broad file access.)
if [[ "${HARDENED:-0}" == "1" ]]; then
	ENTITLEMENTS="${REPO_ROOT}/packaging/macos/Ferail.entitlements"
	codesign --force --options runtime --timestamp \
		--entitlements "${ENTITLEMENTS}" \
		--sign "${IDENTITY}" "${APP_DIR}"
else
	codesign --force --sign "${IDENTITY}" --timestamp=none "${APP_DIR}"
fi
codesign --verify --deep --strict --verbose=2 "${APP_DIR}"

echo
echo "Built ${APP_DIR}"
echo "Run it with:  open \"${APP_DIR}\""
echo
echo "First time you open a protected folder (Desktop/Documents/Downloads/"
echo "removable/network), macOS will show the access prompt. If you ever"
echo "click \"Don't Allow\", macOS won't ask again — use the in-app"
echo "\"Open Full Disk Access settings\" link to grant it manually."
