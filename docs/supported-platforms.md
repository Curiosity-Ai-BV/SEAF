# Supported Platforms

SEAF currently supports local source builds and the supervised local coding
loop on macOS and Linux. Continuous integration exercises Ubuntu, and local
acceptance has been run on macOS.

Use the latest stable Rust toolchain. SEAF does not declare a minimum supported
Rust version (MSRV) yet, and its Cargo manifests intentionally omit
`rust-version`.

The prebuilt release workflow is narrower than source support. Its native matrix
is exactly Ubuntu 22.04 on `x86_64-unknown-linux-gnu` and macOS 15 on
`aarch64-apple-darwin`. Broader Linux, Intel macOS, Windows, musl, and
cross-compiled targets have no release claim. The project does not treat a
successful compile on another operating system or architecture as a
supported-platform result.

The TypeScript SDK and `seaf-local-runtime` are experimental and are not part of
the supported CLI distribution.
