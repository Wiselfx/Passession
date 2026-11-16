# Vendored zbus-secret-service-keyring-store 1.0.1

Source: published crates.io archive `zbus-secret-service-keyring-store-1.0.1.crate`,
SHA-256 `74801d001b9e7729adb4f1825b67b398185fed424749aa3d8bacf70417137d9a`
(upstream `open-source-cooperative/zbus-secret-service-keyring-store` commit
`a97612aa64dd0148ad2504b3a9d4d82ca94e070b`, repository root). The root workspace
patches this crate and excludes the vendor directory. The archive's Cargo.lock,
.cargo_vcs_info.json, .cargo-ok and .gitignore are omitted. `src/`, `examples/`,
`README.md`, `LICENSE-MIT`, `LICENSE-APACHE` and `Cargo.toml.orig` are unchanged;
the manifest declares MIT OR Apache-2.0.

## zbus on Tokio

Upstream depends on `zbus` 5 with default features, which include the `async-io`
executor. `paseo-gui-platform` already builds zbus with `tokio` and without
defaults; Cargo feature unification would otherwise compile the async-io
executor stack into that shared zbus as well. The normalized `Cargo.toml` now
declares the zbus dependency with `default-features = false` and
`features = ["tokio"]`; `Cargo.toml.orig` keeps the upstream declaration.
`secret-service` still enables zbus's `blocking-api`, which the store's
synchronous calls use.

With the Tokio feature, zbus runs blocking calls on its own internal Tokio
runtime, so call the store off Tokio worker threads. Item attributes and the
DH-encrypted Secret Service session are unchanged.
