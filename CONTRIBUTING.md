# Contributing

Thank you for helping. This page is what a change has to satisfy before it is merged.

## Build

```sh
cargo build --workspace
```

The workspace is a Rust crate per layer (`domain`, `app`, `store-sqlite`, `server`, `client`, `agent-host`, the `host-*`
adapters, `cli`) and a small browser bundle under `web/`, embedded in the server at compile time. The toolchain is the
current stable one (`rust-toolchain.toml`).

## The gates

Every one of these passes before a pull request is merged; CI runs them on every push.

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
scripts/check-domain-deps.sh
scripts/check-host-names.sh
scripts/check-clean.sh
cargo xtask integrations --check
npm --prefix web ci
npm --prefix web run typecheck
npm --prefix web test
cargo build -p dibs -p dibs-server && cargo test -p dibs-e2e
npm --prefix web exec playwright install chromium   # once
npm --prefix web run e2e
```

- `scripts/check-domain-deps.sh` keeps the inner layers from importing outer ones.
- `scripts/check-host-names.sh` keeps agent hosts' names out of the core crates and docs, apart from the adapters, the
  integrations, `README.md` and `docs/hosts.md`.
- `scripts/check-clean.sh` fails when a word from a maintainers' list appears in the tree or in the history. Its list is
  not in the repository, so a contributor cannot run it; CI runs it with the list as a secret.
- `cargo xtask integrations --check` verifies that the agents' workflow text, rendered from `docs/agent-workflow.md`
  into every integration, matches its source. Change the source and run `cargo xtask integrations`.
- `tests/e2e/` runs the built `dibs-server` and `dibs` as separate processes on an ephemeral port with a temporary database
  and a home of their own for each member (races between two claimants, the guard's hooks, a project over two
  repositories). `web/e2e/` drives the pages in Chromium with Playwright against the same binary (`DIBS_SERVER_BIN`
  names another build) and fails a test when the page reports a content security policy violation. Both run in CI in
  the `e2e` job; `cargo test --workspace` also runs the first.
- The scripts have tests of their own under `scripts/tests/`, run in CI. The install script's test signs its
  releases with a key it makes, so it needs [minisign](https://jedisct1.github.io/minisign/) installed.
- `deploy/` is validated in CI: every Caddyfile with `caddy validate` in the `caddy:2` image, the compose file with
  `docker compose config`, and the image by building it and starting it. `actionlint` checks the workflows.
- `cargo bench -p dibs` measures a hook on a warm cache, process start included. CI reports it and does not gate on it.

## Rules the code follows

- `unsafe` code is forbidden. `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!` and `dbg!` are denied outside
  tests. `#[allow]`, `#[expect]`, `// @ts-*` comments and `eslint-disable` do not appear in source, and no check is
  silenced to make a gate pass.
- Errors are a `thiserror` enum per crate. The server maps them to `{ "error": { "code", "message" } }` with a stable
  snake_case `code` and an English sentence for the person reading it: never an internal error's text, a SQL message,
  a path or a type name. Details go to the log.
- Time and randomness are ports (`Clock`, `IdGen`, `TokenGenerator`). Nothing in `domain` or `app` reads the system
  clock or the random number generator.
- Behaviour that varies by kind is dispatched through a registry or a trait, not a `match` on the kind in the caller.
- Comments describe the unit they annotate. They do not refer to tickets, plans or other people's work.
- Tests come at two levels: unit tests, which are pure and do no I/O, and integration tests through the real wiring: a
  real SQLite file in a temporary directory, the real router driven with `tower::ServiceExt::oneshot`, a spawned binary
  for end to end. There is no mocked database. A test runs code; it does not read a source or configuration file and
  assert its text.
- The web bundle is plain modules with typed boundaries (`tsc --noEmit` over JSDoc), tested with `node --test`.

### Environment variables

Edition 2024 makes `std::env::set_var` an `unsafe fn`, so no code or test sets environment variables in-process. Every
configuration parser is a pure function over an injected lookup (`impl Fn(&str) -> Option<String>`): binaries pass
`std::env::var`, and tests pass a map. A test of the command line sets variables on the child process
(`assert_cmd`'s `.env()`), never on its own.

### Dependencies

Add a dependency with `cargo add <crate>`, which takes the latest stable version, not a version written from memory.
`cargo deny check` must pass: licences from the allowed list, no advisory, no yanked crate.

## Release signing

A release is published by pushing a tag `v<version>` that matches the workspace's version and the `version` of every
plugin manifest under `integrations/` (a plugin with auto-update on sees a new release only by that string). The release workflow signs the release's `SHA256SUMS` with the project's minisign key and publishes
the signature as `SHA256SUMS.minisig`; `dibs self-update` installs nothing that signature does not cover. The workflow
refuses to publish when the secret is missing or its signature does not verify against `keys/release.pub`, which is
compiled into every binary and written into the released `install.sh` (`scripts/stamp-release-key.sh`), so that
script checks the same signature when minisign is installed.

Until a key is made, `keys/release.pub` holds a placeholder: builds then refuse every self-update, and no release can be
published. To make the key, once, on a trusted machine with [minisign](https://jedisct1.github.io/minisign/)
installed (`brew install minisign`, `apt install minisign`):

```sh
minisign -G -W -p keys/release.pub -s dibs-release.key   # -W: no password, as the workflow signs unattended
gh secret set DIBS_RELEASE_SIGNING_KEY < dibs-release.key  # the repository secret the workflow reads
```

Commit the new `keys/release.pub`. Store `dibs-release.key` in the maintainers' password manager, then delete the file
from the machine; nothing else needs it. Binaries built before the commit trust no key and update only through the
install scripts.

Replacing the key (it leaked, or its holder left) is the same two commands and a commit. Binaries built with the old
key refuse every release signed with the new one, so announce that each member reinstalls with the install script once.

## A public repository

Anything committed here is public. Examples, fixtures, tests, documentation and commit messages use made-up names:
`example.com` hosts, `acme/widgets#42` references, areas such as `billing` and `search`, members such as `alice` and
`bob`. They do not name a real organisation, person or internal system.

## Pull requests from forks

CI runs `scripts/check-clean.sh` with a repository secret, and GitHub does not give secrets to a workflow started by a
pull request from a fork. On a fork's pull request the check is skipped with a notice, so a green run proves less than
it does for a branch of this repository. A maintainer therefore checks a fork's pull request by hand before merging it:
run `scripts/check-clean.sh` on the branch, and read the whole diff, including the commit messages and the author
details of every commit.
