# Release Artifacts

SEAF's release-artifact workflow constructs deterministic, checksummed native
CLI archives. It is intentionally a build-and-verify boundary: it does not
create or push a tag, create a GitHub Release, publish a package, sign, notarize,
or make any artifact durable outside the short-lived workflow run. Explicit
authority for the first tagged prerelease and its publication remains M2-05.

## Supported Prebuilt Matrix

The prebuilt matrix is exactly:

- Ubuntu 22.04: `x86_64-unknown-linux-gnu`
- macOS 15 on Apple Silicon: `aarch64-apple-darwin`

No cross-compiled target is treated as release evidence. Each workflow row
checks that the stable Rust host equals its matrix target before building. For
workspace version `0.1.0`, the workflow accepts only tag `v0.1.0` and binary
identity `seaf 0.1.0` from the checked-out `GITHUB_SHA`.
Identity is a process contract, not a text fragment: `--version` and `info` must
each exit zero, write the exact expected line to stdout, and write nothing to
stderr. The builder, full archive verifier, raw workflow smoke, and installed
workflow smoke all enforce that same rule. Each identity subprocess has its own
64 MiB stdout/stderr file cap; the limit does not escape into later packaging or
workflow commands.

Each row produces one archive:

- `seaf-v0.1.0-x86_64-unknown-linux-gnu.tar.gz`
- `seaf-v0.1.0-aarch64-apple-darwin.tar.gz`

An archive has a same-named root and exactly `CHANGELOG.md`, `LICENSE`,
`README.md`, and executable `seaf`. The builder normalizes entry order, modes,
owners, timestamps, USTAR headers, and the gzip header. Inputs and outputs are
regular non-symlink files bounded to 64 MiB each and 128 MiB in aggregate.
Verification checks the complete normalized gzip header and the USTAR checksum,
link name, device fields, prefix, reserved bytes, member padding, and end
padding before reading any member. Failed construction removes only the exact
script-owned archive and directory; caller-owned output directories remain.

The final `release-assets` directory contains only those two archives and
`SHA256SUMS`. The checksum file has two lexically sorted lowercase SHA-256
lines, uses two spaces before each basename, and ends in one newline.

## Local Verification

Run the same native construction, validation, checksum, extraction, and
installed-binary smoke used by ordinary CI:

```bash
./scripts/test-release-artifacts.sh
```

The test uses external Cargo target, artifact, extraction, and install roots. It
builds the local native binary with locked Cargo, constructs the archive twice,
compares the bytes, validates before extraction, and checks exact `--version`
and `info` output from the installed archive binary. A fixture carrying that
same known executable under the other target name exercises only aggregate
assembly; the tag workflow's native host check is the release authority.

Archive creation has explicit GNU tar and bsdtar branches because their owner
normalization flags differ. The local test directly proves only the host branch;
its static contract locks both option sets, ordinary Ubuntu CI executes GNU tar,
and the macOS 15 workflow row executes bsdtar. Aggregate verification validates
both archives without attempting to execute the foreign-platform binary.

The workflow runs only after a human pushes an exact `v*` tag. It has read-only
repository permission and uploads two-day workflow artifacts. A successful run
is not a published release, and this repository still does not offer public
prebuilt downloads until M2-05 is separately authorized and completed.
