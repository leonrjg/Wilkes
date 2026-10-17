# Building and publishing releases

The **Build** workflow is the single release entry point. A pushed version tag
runs release preparation, the reusable Test workflow, the desktop build matrix,
and native Docker builds for both architectures. Every checkout uses the same
resolved commit SHA. Docker, CI and local Cargo commands use the toolchain pinned
in `rust-toolchain.toml`. Rust builds enforce the committed lockfile; frontend
dependencies are installed with `npm ci`. The GitHub release is created only after these jobs succeed;
container publication requires successful desktop builds and container smoke tests.
The Docker workflow has no independent push or manual-publication trigger.

## Supported artifacts

| Platform | Architecture | Artifacts |
| --- | --- | --- |
| macOS | Apple Silicon (`aarch64`) | DMG |
| Windows | x86-64 | NSIS EXE, MSI, portable ZIP |
| Linux desktop | x86-64 | DEB, AppImage |
| Linux server | amd64 and arm64 | Multi-platform container image |

Intel macOS, ARM Windows, and ARM Linux desktop packages are not built by this
matrix. A successful build is not installation acceptance: install the candidate
packages on the supported operating systems before promoting the release.

## Signing configuration

Set the GitHub repository variable `RELEASE_SIGNING` explicitly to one of:

- `signed`: macOS uses Developer ID signing and notarization, and Windows uses
  Authenticode signing. Missing credentials or failed verification stop the build.
- `unsigned`: macOS uses an ad-hoc signature and Windows artifacts are unsigned.
  This mode is recorded in the GitHub release. These artifacts do not have a
  verified publisher and may require operating-system installation overrides.

No tag can publish while this variable is unset. A manual Build run defaults to
`unsigned` when the variable is unset and never publishes, even when run on a tag.
By default it validates and builds both the desktop packages and container images.

For a Windows packaging retry after a source fix, run the same **Build** workflow
with `windows_only=true` on the corrected branch. This uses the existing Windows
packaging steps and skips the test suite, macOS, Linux, and containers. It never
publishes and can run alongside a full build without cancelling it. Its artifacts
belong to the corrected commit; success is a packaging check, not full release
validation. A rerun of an old failed job always uses the old commit.

```sh
gh workflow run build.yml --ref <corrected-branch> -f windows_only=true
```

For `signed`, configure these repository secrets:

| Secret | Value |
| --- | --- |
| `APPLE_CERTIFICATE` | Base64-encoded Developer ID Application `.p12`, including the private key |
| `APPLE_CERTIFICATE_PASSWORD` | Password protecting that `.p12` |
| `APPLE_SIGNING_IDENTITY` | Full Developer ID Application signing identity |
| `APPLE_ID` | Apple account email used for notarization |
| `APPLE_PASSWORD` | Apple app-specific password for notarization |
| `APPLE_TEAM_ID` | Apple Developer team ID |
| `WINDOWS_CERTIFICATE` | Base64-encoded code-signing `.pfx`, including the private key |
| `WINDOWS_CERTIFICATE_PASSWORD` | Password protecting that `.pfx` |
| `WINDOWS_CERTIFICATE_THUMBPRINT` | Thumbprint of the imported signing certificate |

The macOS job imports its certificate into a temporary keychain, removes the
certificate file, and deletes the keychain after the build. It verifies the app's
signature, notarization ticket, and Gatekeeper assessment. The Windows job removes
the PFX file after import, signs the portable application's loose executables,
and checks the certificates on the installers and executables. Verify installation
of the real bundles separately; these checks do not launch the desktop UI.

See the [Tauri macOS signing instructions](https://v2.tauri.app/distribute/sign/macos/)
for certificate export and notarization requirements.

## Candidate and stable tags

Before tagging, update the Cargo workspace version, workspace entries in
`Cargo.lock`, `ui/package.json`, and both root versions in `ui/package-lock.json`.
A Tauri version override, if added, must agree too. Commit those changes before
creating the tag. Release preparation rejects mismatches.

Stable Windows MSI packages derive their numeric installer version from the
application version. For prereleases with a suffix such as `rc.1`, WiX needs an
explicit numeric `bundle.windows.wix.version` in a platform configuration such
as `crates/desktop/tauri.windows.conf.json`. The `1.0.0-rc.1` candidate used
`1.0.0.1`; that candidate override was removed for stable `1.0.0`. Remove or
update any such override during promotion. Test MSI upgrade behavior separately:
Windows Installer compares only the first three version fields.

Use `v1.0.0-rc.1` with package versions `1.0.0-rc.1` for a candidate. Tags must be
SemVer with a leading `v`; build metadata (`+...`) is not accepted because the
same version must also be a valid Docker tag.

| Tag | GitHub release | Container tags |
| --- | --- | --- |
| `v1.0.0-rc.1` | Prerelease; does not become latest | `1.0.0-rc.1` only |
| `v1.0.0` | Stable; becomes latest | `1.0.0`, `1.0`, `latest` |

Native container images are built, loaded, and tested before pushing. The smoke
test starts the packaged Rust worker with EOF, starts the server with a fresh
data directory, checks settings and frontend responses, then uploads and searches
a text document without model downloads. The manifest merges the exact pushed image digests from this run.
Run-scoped `build-...-amd64` and `build-...-arm64` tags are staging references,
not stable release aliases.

Desktop bundle output is cleared before building. Missing, empty, or duplicate
expected packages fail validation; Windows portable archives must contain the
application, Rust worker, and Python worker resources. The GitHub release includes
`SHA256SUMS`, the validated source SHA, signing mode, and supported platforms.

Releases are serialized to prevent concurrent runs from racing over stable
aliases. GitHub releases and GHCR manifests are separate publication operations;
publication is not transactional across those services. If the final GitHub
release upload fails after a container manifest is published, inspect the run and
rerun the failed jobs for the same tag. Do not move an existing version tag to a
different commit. Rerun manual builds through **Build**, rather than inventing an
independent Docker publication path.

## Local checks

```sh
python3 -m unittest discover -s scripts -p 'test_release*.py' -v
bash -n scripts/smoke-container.sh
actionlint
```

With a running Docker daemon, build and smoke-test the native image:

```sh
docker build -t wilkes-release-check .
bash scripts/smoke-container.sh wilkes-release-check
```

The Test workflow runs the release-tooling checks, the Rust workspace tests
(including the worker), frontend tests with coverage, and the production frontend
build. Pull requests retain their coverage comment; reusable release validation
does not attempt to create one.
