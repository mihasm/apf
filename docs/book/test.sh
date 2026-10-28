#!/bin/sh
# Compiles every Rust code block in the book (`mdbook test`).
#
# The blocks use `apf` and `apf-webview`, so they need the compiled libraries. This builds
# both with cargo (the repository's shared target dir), links exactly the libraries cargo
# used into a temporary folder, and points `mdbook test` at it. Run it from anywhere:
#
#   docs/book/test.sh
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
libs=$(mktemp -d)
trap 'rm -rf "$libs"' EXIT

cd "$root"
cargo build -p apf -p apf-webview -p apf-test --message-format=json |
    sed -n 's/.*"filenames":\[\([^]]*\)\].*/\1/p' | tr ',' '\n' | tr -d '"' |
    grep -E '\.(rlib|rmeta|dylib|so|dll)$' | while read -r file; do ln -sf "$file" "$libs/"; done

CARGO_PKG_VERSION=0.1.0 mdbook test docs/book -L "$libs"
