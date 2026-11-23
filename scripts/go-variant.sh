#!/usr/bin/env bash
# Prepare a build of the Go SDK for the gobench module to run against.
#
#   scripts/go-variant.sh <variant> [ref]
#
# Prints the -modfile path to pass to `go run`. Variants:
#
#   shipped      the module as released (no replace; prints go.mod)
#   rebuilt-s    the ref's source, EQL guest rebuilt with its own profile
#                (opt-level "s"): a control that the local build matches
#                the shipped module
#   noclose      the ref's source with WithCloseOnContextDone(true) removed
#                from encrypt/guest.go; the shipped guest
#   o3           the EQL guest rebuilt at opt-level 3; the shipped host
#   o3-noclose   both
#   local        the stack worktree in $STACK_LOCAL as it stands (its own
#                guest must already be built there): the fix under test
#
# ref defaults to languages/golang/v0.1.0-rc.6. The stack checkout is
# $STACK_REPO (default ~/Projects/CipherStash/stack). Builds land in
# ~/.eqlbench/go-variants/<ref-slug>/<variant>.
set -euo pipefail

variant=${1:?variant}
ref=${2:-languages/golang/v0.1.0-rc.6}
here=$(cd "$(dirname "$0")/.." && pwd)
gomod="$here/go"
stack_repo=${STACK_REPO:-$HOME/Projects/CipherStash/stack}
toolchain=${GUEST_TOOLCHAIN:-1.94.1}
slug=$(echo "$ref" | tr '/' '_')
base="$HOME/.eqlbench/go-variants/$slug"
dir="$base/$variant"
modfile="$gomod/variants/$slug-$variant.mod"

if [ "$variant" = shipped ]; then
  echo "$gomod/go.mod"
  exit 0
fi

mkdir -p "$gomod/variants"

if [ "$variant" = local ]; then
  src=${STACK_LOCAL:?set STACK_LOCAL to a stack checkout}/languages/golang
  for w in eql/wasm/stack_encrypt_guest_eql.wasm encrypttest/wasm/stack_encrypt_guest_eql_deterministic.wasm; do
    test -f "$src/encrypt/$w" || { echo "no $w built in $src" >&2; exit 1; }
  done
  modfile="$gomod/variants/local.mod"
else
  src="$dir/languages/golang"
  if [ ! -d "$dir" ]; then
    mkdir -p "$dir"
    git -C "$stack_repo" archive "$ref" | tar -x -C "$dir"
    guest="$src/encrypt/guest"
    case "$variant" in
      noclose|o3-noclose)
        python3 -I - "$src/encrypt/guest.go" <<'PY'
import sys
p = sys.argv[1]
s = open(p).read()
old = ".\n\t\tWithCloseOnContextDone(true)"
assert old in s, "WithCloseOnContextDone(true) not found where expected"
open(p, "w").write(s.replace(old, ""))
PY
        ;;
    esac
    case "$variant" in
      o3|o3-noclose)
        python3 -I - "$guest/Cargo.toml" <<'PY'
import sys
p = sys.argv[1]
s = open(p).read()
assert 'opt-level = "s"' in s
open(p, "w").write(s.replace('opt-level = "s"', 'opt-level = 3'))
PY
        ;;
    esac
    case "$variant" in
      rebuilt-s|o3|o3-noclose)
        profile=$([ "$variant" = rebuilt-s ] && echo s || echo 3)
        target="$base/target-opt-$profile"
        echo "building EQL guest (opt-level $profile, rust $toolchain) ..." >&2
        # The production EQL guest, and the deterministic-kms EQL guest
        # encrypttest embeds (gobench's offline mode).
        (cd "$guest" && CARGO_TARGET_DIR="$target/eql" cargo "+$toolchain" build -q --locked \
          --target wasm32-wasip1 --release --features eql >&2)
        cp "$target/eql/wasm32-wasip1/release/stack_encrypt_guest.wasm" "$src/encrypt/eql/wasm/stack_encrypt_guest_eql.wasm"
        (cd "$guest" && CARGO_TARGET_DIR="$target/eql-det" cargo "+$toolchain" build -q --locked \
          --target wasm32-wasip1 --release --features eql,deterministic-kms >&2)
        cp "$target/eql-det/wasm32-wasip1/release/stack_encrypt_guest.wasm" "$src/encrypt/encrypttest/wasm/stack_encrypt_guest_eql_deterministic.wasm"
        ;;
    esac
  fi
fi

sed '/^replace /d' "$gomod/go.mod" > "$modfile"
printf '\nreplace github.com/cipherstash/stack/languages/golang => %s\n' "$src" >> "$modfile"
printf 'replace github.com/cipherstash/stack/languages/golang/encrypt/encrypttest => %s/encrypt/encrypttest\n' "$src" >> "$modfile"
cp "$gomod/go.sum" "${modfile%.mod}.sum"
# Another ref may pin other vitaminc bindings than rc.6 does.
(cd "$gomod" && go mod tidy -modfile="$modfile" >&2)
echo "$modfile"
