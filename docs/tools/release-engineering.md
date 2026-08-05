# Release engineering

Reimer uses three independent GitHub Actions workflows:

- **CI** validates Rust, the VS Code extension, and native linking on Linux, Windows, and macOS.
- **Documentation** builds mdBook on pull requests and deploys it to GitHub Pages after a protected branch is updated.
- **Release** validates an existing tag, builds native archives, packages the Windows VSIX, writes SHA-256 files, and creates a GitHub Release.

## One-time repository setup

1. Push the repository to `https://github.com/Scr44gr/reimer`.
2. Open **Settings → Pages**.
3. Under **Build and deployment**, choose **GitHub Actions** as the source.
4. Protect `master` or `main` and require the CI and documentation build jobs before merge.

No `gh-pages` branch is required. The Pages workflow uploads `target/book` and deploys through GitHub's `github-pages` environment.

## Prepare a version

Keep these versions identical:

- `[workspace.package].version` in `Cargo.toml`;
- `version` in `editors/vscode/package.json`;
- the root package version in `editors/vscode/package-lock.json`;
- the Git tag without its `v` prefix.

Run the permanent gates locally:

```text
cargo fmt --all --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -W clippy::perf -W clippy::redundant_clone -W clippy::needless_collect -D warnings
```

Then verify the tag before creating it:

```powershell
.\scripts\release\verify-tag.ps1 -Tag v0.1.1
```

## Publish from a tag

```text
git tag -a v0.1.1 -m "Reimer v0.1.1"
git push origin v0.1.1
```

The tag must already exist and point to the code being released. The workflow never creates or moves a tag.

It produces archives for:

- Linux x64 and arm64;
- Windows x64;
- macOS Intel and Apple silicon;
- a self-contained Windows VS Code extension.

Each compiler archive contains `reimer`, `reimer-lsp`, `reimer-lint`, and the matching `std` directory. The binaries and standard library are kept together so standard imports work outside a source checkout.

The final job verifies the tag through GitHub's API, creates the release when needed, and uploads every archive, checksum, and VSIX as durable release assets. It validates the published asset list and writes the release URL plus direct build links to the job summary.

## Manual rerun

The release workflow supports **Run workflow** with an existing tag. This is for recovering from transient CI infrastructure failures or completing a release whose build assets were not published. Existing assets with matching names are replaced, while the release title and notes are preserved.

## Current trust model

Releases are unsigned experimental binaries with SHA-256 checksums. Code signing and GitHub artifact attestations can be added when stable publisher identities and signing credentials exist. Secrets should never be added merely to make an experimental build appear signed.
