# Rezed

[![Rezed](https://img.shields.io/endpoint?url=https://raw.githubusercontent.com/nguyenphutrong/rezed/rezed/assets/badge/v0.json)](https://github.com/nguyenphutrong/rezed)
[![CI](https://github.com/nguyenphutrong/rezed/actions/workflows/run_tests.yml/badge.svg?branch=rezed)](https://github.com/nguyenphutrong/rezed/actions/workflows/run_tests.yml)

Rezed is a fork of [Zed](https://github.com/zed-industries/zed), the high-performance, multiplayer code editor from the creators of [Atom](https://github.com/atom/atom) and [Tree-sitter](https://github.com/tree-sitter/tree-sitter).

---

### Installation

Rezed releases are published from this fork's [GitHub Releases](https://github.com/nguyenphutrong/rezed/releases) when tags are pushed.

Until fork-specific installers are fully established, upstream Zed installation documentation remains useful for platform prerequisites:

- [Building on macOS](./docs/src/development/macos.md)
- [Building on Linux](./docs/src/development/linux.md)
- [Building on Windows](./docs/src/development/windows.md)

Other platforms are not yet available:

- Web ([tracking discussion](https://github.com/zed-industries/zed/discussions/26195))

### Developing Rezed

The internal Rust crate and binary names are still inherited from upstream Zed for now. Start with the upstream build commands documented above, then use the fork CI as the source of truth for supported checks.

### Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md) for upstream contribution context. Fork-specific contribution and release rules will live in this README as they diverge.

### Licensing

Rezed source code is licensed primarily under GPL-3.0-or-later, with Apache-2.0 components where marked.

License information for third party dependencies must be correctly provided for CI to pass.

We use [`cargo-about`](https://github.com/EmbarkStudios/cargo-about) to automatically comply with open source licenses. If CI is failing, check the following:

- Is it showing a `no license specified` error for a crate you've created? If so, add `publish = false` under `[package]` in your crate's Cargo.toml.
- Is the error `failed to satisfy license requirements` for a dependency? If so, first determine what license the project has and whether this system is sufficient to comply with this license's requirements. If you're unsure, ask a lawyer. Once you've verified that this system is acceptable add the license's SPDX identifier to the `accepted` array in `script/licenses/zed-licenses.toml`.
- Is `cargo-about` unable to find the license for a dependency? If so, add a clarification field at the end of `script/licenses/zed-licenses.toml`, as specified in the [cargo-about book](https://embarkstudios.github.io/cargo-about/cli/generate/config.html#crate-configuration).

## Relationship to Upstream

Rezed is not affiliated with Zed Industries, Inc. Upstream Zed remains the base project and copyright holder for inherited code.

Fork-specific CI/CD intentionally avoids Zed Industries' private runners, secrets, deployment targets, Slack/Discord automation, Sentry project, and release publishing steps.
