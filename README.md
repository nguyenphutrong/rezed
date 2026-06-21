# Rezed

[![Rezed](https://img.shields.io/endpoint?url=https://raw.githubusercontent.com/nguyenphutrong/rezed/rezed/assets/badge/v0.json)](https://github.com/nguyenphutrong/rezed)
[![CI](https://github.com/nguyenphutrong/rezed/actions/workflows/run_tests.yml/badge.svg?branch=rezed)](https://github.com/nguyenphutrong/rezed/actions/workflows/run_tests.yml)

Rezed is a community-maintained fork of [zed-industries/zed](https://github.com/zed-industries/zed), focused on a Git-first editing experience for developers who live in branches, pull requests, diffs, history, and review workflows.

The fork keeps Zed's performance-oriented editor foundation while giving community contributors room to experiment with deeper Git features and practical source-control ergonomics.

---

### Focus

Rezed prioritizes Git workflows that are useful inside the editor:

- Faster navigation through commit history, branches, and changed files
- Clearer diff and review surfaces for day-to-day development
- Better workflows around branch comparison, pull request preparation, and repository state
- Community-driven iteration on Git features that may be too experimental for upstream

### Status

Rezed is maintained by community contributors. The project is in an early fork setup phase: branding, CI, release packaging, and fork-specific documentation are being established incrementally.

The internal Rust crate and binary names are still inherited from upstream Zed for now.

### Installation

Rezed releases are published from this fork's [GitHub Releases](https://github.com/nguyenphutrong/rezed/releases) when tags are pushed.

Until fork-specific installers are fully established, use the development setup below to build locally.

### Developing Rezed

- [Building on macOS](./docs/src/development/macos.md)
- [Building on Linux](./docs/src/development/linux.md)
- [Building on Windows](./docs/src/development/windows.md)

Use the fork CI as the source of truth for supported checks.

### Contributing

Contributions are welcome, especially around Git features, editor source-control workflows, fork packaging, and documentation.

For inherited development practices, see [CONTRIBUTING.md](./CONTRIBUTING.md). Fork-specific contribution and release rules will live in this README as they diverge.

### Licensing

Rezed source code is licensed primarily under GPL-3.0-or-later, with Apache-2.0 components where marked.

License information for third party dependencies must be correctly provided for CI to pass.

We use [`cargo-about`](https://github.com/EmbarkStudios/cargo-about) to automatically comply with open source licenses. If CI is failing, check the following:

- Is it showing a `no license specified` error for a crate you've created? If so, add `publish = false` under `[package]` in your crate's Cargo.toml.
- Is the error `failed to satisfy license requirements` for a dependency? If so, first determine what license the project has and whether this system is sufficient to comply with this license's requirements. If you're unsure, ask a lawyer. Once you've verified that this system is acceptable add the license's SPDX identifier to the `accepted` array in `script/licenses/zed-licenses.toml`.
- Is `cargo-about` unable to find the license for a dependency? If so, add a clarification field at the end of `script/licenses/zed-licenses.toml`, as specified in the [cargo-about book](https://embarkstudios.github.io/cargo-about/cli/generate/config.html#crate-configuration).

## Relationship to Upstream

Rezed is sourced from [zed-industries/zed](https://github.com/zed-industries/zed) and is not affiliated with Zed Industries, Inc. Upstream Zed remains the base project and copyright holder for inherited code.

Fork-specific CI/CD intentionally avoids Zed Industries' private runners, secrets, deployment targets, Slack/Discord automation, Sentry project, and release publishing steps.
