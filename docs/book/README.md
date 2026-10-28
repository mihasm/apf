# The APF book

The sources of the book *Building audio plugins with APF*, written for [mdBook](https://rust-lang.github.io/mdBook/).
The chapters are in `src/`, listed in `src/SUMMARY.md`.

## Read it

Install mdBook once (`brew install mdbook`, or `cargo install mdbook --locked`), then from the
repository root:

```sh
mdbook serve docs/book --open    # build, serve at http://localhost:3000 and reload on every save
mdbook build docs/book           # write the HTML into docs/book/book/ (not committed)
```

## Check it

The example chapters include the real source files of `examples/` (`{{#include ...}}`), so the
code in the book is the code that is tested. The other Rust blocks in the book are compiled
by `mdbook test`. They need the compiled `apf` libraries, which `test.sh` prepares:

```sh
docs/book/test.sh
```

It builds `apf`, `apf-webview` and `apf-test` with cargo, then runs `mdbook test` against
them. A block that cannot compile on its own (it includes an example that embeds a web page
or presets) is marked `ignore`; that example's own `cargo test` covers it.

## Publish it

The book is served by GitHub Pages from the `gh-pages` branch at
<https://mihasm.github.io/apf/>. There is no CI: after changing the book, run
`scripts/publish-book.sh`. It builds the book and pushes the result to `gh-pages`.
