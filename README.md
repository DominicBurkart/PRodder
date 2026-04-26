# PRodder

[![CI](https://github.com/DominicBurkart/PRodder/actions/workflows/ci.yml/badge.svg)](https://github.com/DominicBurkart/PRodder/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/DominicBurkart/PRodder/graph/badge.svg)](https://codecov.io/gh/DominicBurkart/PRodder)
[![license](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/DominicBurkart/PRodder#license)
[![last commit](https://img.shields.io/github/last-commit/dominicburkart/prodder)](https://github.com/DominicBurkart/PRodder)

PR lifecycle management that demotes failing open PRs to drafts.

## Use-case

For projects requiring human review, preserve human attention by demoting open PRs that don't pass repo merge requirements to drafts. [Here's the flow PRodder was built for originally](https://dominic.computer/blog/2026/routines), but it can be applied to a variety of agentic workflows.

## Quickstart

```sh
git clone https://github.com/DominicBurkart/PRodder/ && cd PRodder && GH_KEY=[classic PAT with repo write] cargo run
```

## Type: Job

PRodder runs as a one-shot job. The reference deployment runs in a [Scaleway serverless container job](https://www.scaleway.com/en/serverless-jobs/).

## Targets

Native builds (Linux/macOS/Windows, x86_64 and aarch64) are tier-1. `wasm32-wasip1` and `wasm32-unknown-unknown` compile (`cargo check`) in CI on a non-blocking basis — tracked in [#19](https://github.com/DominicBurkart/PRodder/issues/19). The browser-WASM `GH_TOKEN` injection story is deferred until an HTTP client with a browser-compatible backend lands.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
