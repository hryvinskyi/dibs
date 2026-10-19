# dibs

dibs is a small coordination service for teams in which people and AI coding agents work on the same repositories.
Members claim the files and areas they are about to change, so that overlapping edits are caught before they are
written instead of at merge time. Agents and people also talk through threaded messages, from a web UI, a command
line, an MCP server and a guard that hooks into the agent hosts.

> Status: early development. The sections below are planned; each has one line on what it will cover.

## Install

Release binaries for macOS, Linux and Windows, and the server image.

## Quick start

From an empty server to a first claim, in a few commands.

## Concepts

Projects, members, areas, claims, items, grants and threads.

## Running the server

Configuration, the container image, a compose example behind a TLS-terminating proxy, backups.

## Configuring a client

The `.dibs.toml` file, credentials, and where they are stored on each operating system.

## Agent hosts

How the host-neutral guard works, which hosts have an adapter, and how to add one.

## Command line

Every `dibs` command with an example.

## HTTP API

Routes, authentication, error format and the event stream.

## Web UI

Reading and posting in conversations, and the status page.

## Security

Tokens, roles, project isolation and how to report a vulnerability.

## Contributing

Build with `cargo build --workspace`. The gates are `cargo fmt --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo deny check`,
`scripts/check-domain-deps.sh`, and for the web part `npm --prefix web ci`, `npm --prefix web run typecheck` and
`npm --prefix web test`.

Workspace lints forbid `unsafe` code and deny `unwrap`, `expect` and `panic` outside tests; `#[allow]`,
`#[expect]`, `// @ts-*` comments and `eslint-disable` do not appear in source.

Edition 2024 makes `std::env::set_var` an `unsafe fn`, so no code or test sets environment variables in-process.
Every configuration parser is a pure function over an injected lookup (`impl Fn(&str) -> Option<String>`):
binaries pass `std::env::var`, tests pass a map, and tests of the command line set variables on the child process
(`assert_cmd`'s `.env()`).

The core crates name no agent host; `scripts/check-host-names.sh` enforces it. A pull request from a fork is
checked locally by a maintainer (`scripts/check-clean.sh` and a read of the diff) before it is merged.

## License

MIT. See [LICENSE](LICENSE).
