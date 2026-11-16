# Vendored node-semver 2.2.0

Source: published crates.io archive `node-semver-2.2.0.crate`, SHA-256
`3b1a233ea5dc37d2cfba31cfc87a5a56cc2a9c04e3672c15d179ca118dae40a7`
(upstream `felipesere/node-semver-rs` commit
`aba6daefdb3a84b4966bfb74d9bc8b96642e35ed`, repository root). The root workspace
patches this crate and excludes the vendor directory. `README.md`, `LICENSE`
and `Cargo.toml.orig` are unchanged; benches, the changelog and release tooling
are omitted.

## No miette

Upstream implements `miette::Diagnostic` for `SemverError` and `SemverErrorKind`,
which compiles miette, miette-derive and unicode-width 0.1 into the daemon and
plugin SDK for diagnostic metadata Paseo never renders. The patch removes that
implementation, the `#[diagnostic]` attributes and `SemverError::span()`;
`SemverError` stores the byte offset that `offset()` and `location()` already
returned. The nom parser, range semantics, `Display` text and `Error` impls are
unchanged. The normalized `Cargo.toml` drops miette and the bench-only criterion
dependency.

To update, replace `src/` with the newer published crate and re-apply the
changes above.
