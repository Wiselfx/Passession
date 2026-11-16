# Vendored portable-pty 0.9.0

Source: published crates.io archive `portable-pty-0.9.0.crate`,
SHA-256 `b4a596a2b3d2752d94f51fac2d4a96737b8705dddd311a32b9af47211f08671e`
(upstream `wezterm/wezterm` commit `f8921727a11b9f8b073e8c24821d72fd41283500`,
path `pty`). 0.9.0 is the latest published release. The root workspace patches
this crate and excludes the vendor directory. The archive's Cargo.lock,
.cargo_vcs_info.json and .cargo-ok are omitted. `src/`, `examples/`,
`LICENSE.md` and `Cargo.toml.orig` are unchanged; the manifest declares MIT.

## Shared nix and winreg

Upstream pins `nix` 0.28 and, on Windows, `winreg` 0.10. The daemon already
builds `nix` 0.31, and `embed-resource` builds `winreg` 0.56 for the Windows
client resources, so the published crate compiled a second copy of each. The
normalized `Cargo.toml` now requires `nix` 0.31 (same `term` and `fs` features)
and `winreg` 0.56; `Cargo.toml.orig` keeps the upstream declarations.

No source changes were needed. The crate calls `nix::unistd::access` and
`nix::sys::termios::tcgetattr`, whose signatures are unchanged in 0.31.
`winreg` 0.56 keeps `RegKey::predef`, `open_subkey`, `enum_values`, the public
`RegValue::{bytes, vtype}` fields (`bytes` is now a `Cow<[u8]>` read through
the same deref) and the `OsString` decoding that trims trailing NULs, so the
Windows registry environment merge behaves as before. `MasterPty::get_termios`
returns the `nix` 0.31 `Termios` type.

To update, replace the crate with the newer published archive and drop these
version edits once upstream requires `nix` and `winreg` versions the workspace
already builds.
