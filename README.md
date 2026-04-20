# PRodder

[![CI](https://github.com/DominicBurkart/PRodder/actions/workflows/ci.yml/badge.svg)](https://github.com/DominicBurkart/PRodder/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/DominicBurkart/PRodder/graph/badge.svg)](https://codecov.io/gh/DominicBurkart/PRodder)
[![license](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/DominicBurkart/PRodder#license)
[![last commit](https://img.shields.io/github/last-commit/dominicburkart/prodder)](https://github.com/DominicBurkart/PRodder)

PR lifecycle management that demotes failing open PRs to drafts.

## Use-case

For projects requiring human review, preserve human attention by demoting open PRs that don't pass repo merge requirements to drafts.

## Quickstart

```sh
git clone https://github.com/DominicBurkart/PRodder/ && cd PRodder && GH_KEY=[classic PAT with repo write] cargo run
```

## Tokens

### Classic PATs (recommended)

A classic personal access token with the `repo` scope is the simplest
setup: one token covers every repository owned by the authenticating
user, so PRodder's cross-repo `convertPullRequestToDraft` mutation
works without further configuration.

### Fine-grained PATs (partial support)

Fine-grained PATs are scoped per-repository. PRodder's search step
works fine (search only needs metadata read), but the
`convertPullRequestToDraft` GraphQL mutation requires the token to
have **`Pull requests: Read and write`** permission on **each
repository** it should operate on — otherwise GitHub returns
`FORBIDDEN` / `"Resource not accessible by personal access token"`
(tracked in [issue #17](https://github.com/DominicBurkart/PRodder/issues/17)).

When PRodder encounters this, it emits a dedicated WARN log of the form:

```
convertPullRequestToDraft forbidden on {owner}/{repo}#{n}: token lacks pull_requests:write — grant per-repo or use classic PAT
```

If you want to use a fine-grained PAT for the reference
`DominicBurkart/*` deployment, the token needs `Pull requests: Read
and write` on each of the following active repositories:

- [DominicBurkart/PRodder](https://github.com/DominicBurkart/PRodder)
- [DominicBurkart/marigold](https://github.com/DominicBurkart/marigold)
- [DominicBurkart/nanna-coder](https://github.com/DominicBurkart/nanna-coder)
- [DominicBurkart/velib-mcp](https://github.com/DominicBurkart/velib-mcp)
- [DominicBurkart/htn](https://github.com/DominicBurkart/htn)
- [DominicBurkart/turbolift](https://github.com/DominicBurkart/turbolift)

(Add any other `DominicBurkart/*` repo that you expect to have open
PRs during a given cycle.) If per-repo enumeration is impractical,
fall back to a classic PAT.

See GitHub's docs for details on creating fine-grained PATs and
selecting per-repository permissions:
<https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens#creating-a-fine-grained-personal-access-token>.

## Type: Job

PRodder runs as a one-shot job. The reference deployment runs in a [Scaleway serverless container job](https://www.scaleway.com/en/serverless-jobs/).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
