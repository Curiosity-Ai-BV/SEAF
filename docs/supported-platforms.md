# Supported Platforms

SEAF currently supports local source builds and the supervised local coding
loop on macOS and Linux. Continuous integration exercises Ubuntu, and local
acceptance has been run on macOS.

Use the latest stable Rust toolchain. SEAF does not declare a minimum supported
Rust version (MSRV) yet, and its Cargo manifests intentionally omit
`rust-version`.

Windows is not supported. SEAF also does not claim support for any specific
processor architecture beyond the environments covered by the evidence above.
The project does not treat a successful compile on another operating system or
architecture as a supported-platform result.

The TypeScript SDK and `seaf-local-runtime` are experimental and are not part of
the supported CLI distribution.
