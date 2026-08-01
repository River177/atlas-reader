#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  exit 0
fi

binary="${1:-target/release/atlas-web}"
identifier="com.atlasreader.desktop"

if [[ ! -f "$binary" ]]; then
  printf 'Atlas web server binary not found: %s\n' "$binary" >&2
  exit 1
fi

identity="${ATLAS_LOCAL_SIGNING_IDENTITY:-}"
if [[ -z "$identity" ]]; then
  identity="$(
    security find-identity -v -p codesigning |
      sed -n 's/^[[:space:]]*[0-9][0-9]*) [0-9A-F]\{40\} "\(Apple Development:.*\)"$/\1/p' |
      head -n 1
  )"
fi
if [[ -z "$identity" ]]; then
  printf '%s\n' \
    'No Apple Development signing identity is available.' \
    'Install one through Xcode, or set ATLAS_LOCAL_SIGNING_IDENTITY explicitly.' >&2
  exit 1
fi

codesign \
  --force \
  --sign "$identity" \
  --identifier "$identifier" \
  --timestamp=none \
  "$binary"
codesign --verify --strict "$binary"

metadata="$(codesign -dvvv -r- "$binary" 2>&1)"
signed_identifier="$(
  printf '%s\n' "$metadata" |
    sed -n 's/^Identifier=//p' |
    head -n 1
)"
signed_team="$(
  printf '%s\n' "$metadata" |
    sed -n 's/^TeamIdentifier=//p' |
    head -n 1
)"
requirement="$(printf '%s\n' "$metadata" | tail -n 1 | sed 's/^# //')"
if [[ "$signed_identifier" != "$identifier" || -z "$signed_team" || "$signed_team" == "not set" ]]; then
  printf 'Atlas web signature does not have the expected identifier and Team ID\n' >&2
  exit 1
fi
if [[ "$requirement" != *'anchor apple generic'* ]]; then
  printf 'Atlas web signature is not anchored to Apple: %s\n' "$requirement" >&2
  exit 1
fi

printf 'Locally signed Atlas web server as %s for team %s\n' "$identifier" "$signed_team"
