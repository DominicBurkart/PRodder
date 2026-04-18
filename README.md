# PRodder

[![CI](https://github.com/DominicBurkart/PRodder/actions/workflows/ci.yml/badge.svg)](https://github.com/DominicBurkart/PRodder/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/DominicBurkart/PRodder/graph/badge.svg)](https://codecov.io/gh/DominicBurkart/PRodder)
[![license](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/DominicBurkart/PRodder#license)
[![last commit](https://img.shields.io/github/last-commit/dominicburkart/prodder)](https://github.com/DominicBurkart/PRodder)

PR lifecycle management that demotes failing open PRs to drafts.

## Use-case

> For projects requiring human review, preserve human attention by demoting open PRs that don't pass repo merge requirements to drafts.

## Quickstart

```sh
git clone https://github.com/DominicBurkart/PRodder/ && cd prodder && GH_KEY= cargo run
```

## Type: Job

PRodder runs as a one-shot job. The reference deployment runs in a [Scaleway serverless container job](https://www.scaleway.com/en/serverless-jobs/).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
