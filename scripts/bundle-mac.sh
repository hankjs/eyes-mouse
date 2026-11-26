#!/bin/sh
set -eu
cd "$(dirname "$0")/.."

if [ "$#" -ne 0 ]; then
  printf '%s\n' 'Run npm run bundle:mac without additional Tauri arguments.' >&2
  exit 1
fi

identity_file="$HOME/Library/Application Support/IngridMeow/signing-identity"
identity=$(printf '%s' "${APPLE_SIGNING_IDENTITY-}" | tr 'abcdef' 'ABCDEF')
if [ -f "$identity_file" ]; then
  pinned_identity=$(tr 'abcdef' 'ABCDEF' < "$identity_file")
  if [ "${APPLE_SIGNING_IDENTITY+x}" = x ] && [ "$identity" != "$pinned_identity" ]; then
    printf 'APPLE_SIGNING_IDENTITY conflicts with the fingerprint saved in %s.\n' "$identity_file" >&2
    exit 1
  fi
  identity=$pinned_identity
fi

case $identity in
  *[!0123456789abcdefABCDEF]*) identity= ;;
esac
if [ "${#identity}" -ne 40 ]; then
  printf '%s\n' 'Set APPLE_SIGNING_IDENTITY or save the 40-character certificate fingerprint in:' \
    "  $identity_file" \
    'List valid signing identities with: security find-identity -v -p codesigning' >&2
  exit 1
fi

identities=$(security find-identity -v -p codesigning)
if ! printf '%s\n' "$identities" | awk -v fingerprint="$identity" '$2 == fingerprint { found = 1 } END { exit !found }'; then
  printf 'Signing identity %s is unavailable. Check its certificate and private key in Keychain Access.\n' "$identity" >&2
  exit 1
fi

# Keep the selected Keychain identity when Tauri's certificate-import variables are set.
unset APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD
export APPLE_SIGNING_IDENTITY="$identity"

target_dir=$(cd src-tauri && cargo metadata --no-deps --format-version 1 |
  node -p 'JSON.parse(require("node:fs").readFileSync(0, "utf8")).target_directory')
target=aarch64-apple-darwin
app="$target_dir/$target/release/bundle/macos/IngridMeow.app"

sh tracker/setup.sh
sh tracker/bundle.sh
tauri build --target "$target" --config src-tauri/tauri.bundle.json --bundles app

signer_requirement="certificate leaf = H\"$identity\""
app_requirement="identifier \"com.hankjs.gazemouse\" and $signer_requirement"
codesign --verify --strict --deep -R="$app_requirement" "$app"
codesign --verify --strict -R="$signer_requirement" "$app/Contents/MacOS/eyes-mouse-tracker"
printf 'Verified the IngridMeow bundle and tracker signatures: %s\n' "$app"
