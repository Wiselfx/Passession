# Vendored tiny-skia 0.11.4

Source: published crates.io archive `tiny-skia-0.11.4.crate`, SHA-256
`83d13394d44dae3207b52a326c0c85a8bf87f1541f23b0d143811088497b09ab`
(upstream `RazrFalcon/tiny-skia` commit `ce3fe9bab0659fb928d6c22c03b74e009a8a2b77`).
The root workspace patches this crate and excludes the vendor directory. The
archive's Cargo.lock, .cargo_vcs_info.json, .cargo-ok, .cargo/, .github/ and
.gitignore are omitted. LICENSE (BSD-3-Clause) and everything else are kept.

## PNG on png 0.18

`resvg` 0.45 requires `tiny-skia` ^0.11.4 with default features, which include
`png-format`, and every 0.11 release depends on `png` 0.17. The rest of the GUI
(`image`, Makepad, the tray and clipboard code) uses `png` 0.18, so this crate
alone kept `png` 0.17 in the lockfile. `Cargo.toml` now requires `png` 0.18;
`Cargo.toml.orig` is unchanged. Features stay as published, so `png-format`
and the PNG API remain available.

`Pixmap::decode_png` and `Mask::decode_png` (`src/pixmap.rs`, `src/mask.rs`)
take the two changes tiny-skia 0.12.0 made for png 0.18:

- `png::Decoder` needs `BufRead + Seek`, so the slice is wrapped in
  `std::io::Cursor`. Upstream also adds a `BufReader`; a `Cursor<&[u8]>` is
  already buffered, so it is left out.
- `Reader::output_buffer_size` returns `None` when the frame cannot fit in
  memory; that maps to `png::DecodingError::LimitsExceeded`.

Transformations, the 8-bit and indexed checks, RGB/gray expansion,
premultiplication and every custom error message are unchanged. Encoding calls
are source-compatible. png 0.18's encoder defaults (`Compression::Balanced`,
adaptive filtering) can produce different PNG bytes than 0.17 for the same
pixels; the decoded pixels are identical.

Rasterization code is untouched. When updating, replace from the published
crate and reapply the two `decode_png` hunks, or move to a tiny-skia release on
png 0.18 once `resvg` accepts one.
