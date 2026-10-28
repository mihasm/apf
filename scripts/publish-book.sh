#!/bin/sh
# Build the book (docs/book) and publish it to the gh-pages branch, which GitHub Pages serves
# at https://mihasm.github.io/apf/. There is no CI: run this by hand after changing the book.
# Needs mdBook (brew install mdbook) and push access to origin.
set -eu
root=$(git rev-parse --show-toplevel)
rev=$(git -C "$root" rev-parse --short HEAD)
name=$(git -C "$root" config user.name)
email=$(git -C "$root" config user.email)
url=$(git -C "$root" remote get-url origin)

mdbook build "$root/docs/book"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
if git clone --quiet --branch gh-pages --single-branch "$url" "$tmp/site" 2>/dev/null; then
  :
else
  git init --quiet -b gh-pages "$tmp/site"
  git -C "$tmp/site" remote add origin "$url"
fi
rsync -a --delete --exclude .git "$root/docs/book/book/" "$tmp/site/"
touch "$tmp/site/.nojekyll"
cd "$tmp/site"
git add -A
if git diff --cached --quiet; then
  echo "The published book is already up to date."
  exit 0
fi
git -c user.name="$name" -c user.email="$email" commit --quiet -m "Publish the book (source $rev)"
git push --quiet origin gh-pages
echo "Published: https://mihasm.github.io/apf/"
