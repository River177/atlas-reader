#!/usr/bin/env bash

set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
generated_dir="$repository_root/packages/contracts/src/generated"
temporary_dir="$(mktemp -d)"

cleanup() {
  rm -rf -- "$temporary_dir"
}
trap cleanup EXIT

(
  cd "$repository_root"
  TS_RS_EXPORT_DIR="$temporary_dir" "$HOME/.cargo/bin/cargo" test -p atlas-domain export_bindings
)

diff -ru "$generated_dir" "$temporary_dir"
