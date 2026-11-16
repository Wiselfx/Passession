# Vendored makepad-vector 1.0.0

Source: published crates.io archive `makepad-vector-1.0.0.crate`, SHA-256
`cdc7f49b4be2f20c94c3bddaca1bdad02ca2b18687d0b3cb5f574b8a52aae3ee`
(upstream `makepad/makepad` commit `a1868e19d3bb955200957efb3c880791a43424eb`,
path `draw/vector`). The root workspace patches this crate and excludes the
vendor directory. The archive's Cargo.lock, .cargo_vcs_info.json and .cargo-ok
are omitted. The published archive omits a license file; LICENSE retains
Makepad's upstream MIT license, identical to the existing makepad-draw and
makepad-platform vendor licenses. The manifest declares MIT OR Apache-2.0.

## Shared resvg and ttf-parser

Upstream depends on `resvg` 0.42 with default features off and on
`makepad-ttf-parser` 0.21.1, Makepad's ttf-parser fork. The GUI already resolves
`resvg` 0.45 and the standard `ttf-parser` 0.25 (through makepad-draw's
`rustybuzz` 0.20 and resvg's text stack), so the lockfile carried two
resvg/usvg stacks and two ttf-parser copies. `Cargo.toml` now requires `resvg`
0.45.1, still without default features, and `ttf-parser` 0.25 in place of
`makepad-ttf-parser`. `Cargo.toml.orig` is unchanged.

The crate only names `usvg::Tree` and calls `Tree::from_data(data,
&Options::default())` for OpenType SVG glyph documents. Both are unchanged
between 0.42 and 0.45; 0.45 adds `Options::style_sheet`, which defaults to
`None`. Cargo unifies the GUI's `text` feature into the shared crate. The default
`Options` font database is empty, so `<text>` inside an SVG glyph document still
renders nothing, as it did without the feature.

## Source changes

- `src/ttf_parser/mod.rs` drops `use makepad_ttf_parser as ttf_parser;`, so its
  `ttf_parser::` paths and the reexported `Face`, `FaceParsingError` and
  `GlyphId` name the standard crate.
- `src/path/path.rs` and `src/path/line_path.rs` (`Commands<'_>`) and
  `src/trapezoidator/mod.rs` (`Trapezoidate<'_>`) spell elided return
  lifetimes, as Rust's `mismatched_lifetime_syntaxes` lint asks; behavior is
  unchanged.
- `src/ttf_parser/tests/test.rs` is removed. No module declares it, so it was
  never compiled, and it named the removed fork.

All other source files match the published archive.
