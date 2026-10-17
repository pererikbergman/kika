#!/bin/sh
# Renders the Homebrew formula from kika.rb.tmpl to stdout.
# Usage: render-formula.sh <version> <sha-macos-arm> <sha-macos-intel> <sha-linux-arm> <sha-linux-intel>
# The release workflow and tests/release_workflow.rs both call this script, so
# the formula the tap gets is the one the tests check.
set -eu

if [ "$#" -ne 5 ]; then
  echo "usage: $0 <version> <sha-macos-arm> <sha-macos-intel> <sha-linux-arm> <sha-linux-intel>" >&2
  exit 2
fi

version=$1
if ! printf '%s\n' "$version" | grep -Eqx '[0-9]+\.[0-9]+\.[0-9]+'; then
  echo "error: version '$version' is not X.Y.Z" >&2
  exit 1
fi

shift
for sha in "$@"; do
  if ! printf '%s\n' "$sha" | grep -Eqx '[0-9a-f]{64}'; then
    echo "error: '$sha' is not a 64-character hex sha256" >&2
    exit 1
  fi
done

template="$(dirname "$0")/kika.rb.tmpl"
# Every value is checked above to hold only digits, dots and hex, so none can
# break out of the sed replacement.
rendered=$(sed \
  -e "s/@VERSION@/$version/g" \
  -e "s/@SHA256_MACOS_ARM@/$1/g" \
  -e "s/@SHA256_MACOS_INTEL@/$2/g" \
  -e "s/@SHA256_LINUX_ARM@/$3/g" \
  -e "s/@SHA256_LINUX_INTEL@/$4/g" \
  "$template")

if printf '%s\n' "$rendered" | grep -Eq '@[A-Z0-9_]+@'; then
  echo "error: placeholders left in the rendered formula:" >&2
  printf '%s\n' "$rendered" | grep -E '@[A-Z0-9_]+@' >&2
  exit 1
fi

printf '%s\n' "$rendered"
