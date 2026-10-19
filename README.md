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

Every agent host reaches dibs through MCP (`dibs mcp`, the same for all of them) and through its hooks, which call
`dibs hook <host> <event>`. The guard behind the hooks is one for every host; each host has a small adapter between its
hook format and the guard's. [docs/hosts.md](docs/hosts.md) has the neutral protocol, the capabilities and how to add
a host.

| Host | Status | What its guard will see |
|---|---|---|
| Claude Code | the first supported host; its adapter and plugin are in development | session start, every tool call before and after, prompts; edits and shell commands can be refused |
| Codex CLI | planned | its command hooks |
| OpenCode | planned | its in-process plugins, through a shim over `dibs hook neutral` |

`dibs doctor` shows the hosts this binary supports and what each can carry. `DIBS_OFF=1` switches the guard off on a
machine: every hook answers without objection and contacts nobody.

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
`scripts/check-domain-deps.sh`, `cargo xtask integrations --check` (the agents' workflow text, rendered from
`docs/agent-workflow.md` into every integration, matches its source), and for the web part `npm --prefix web ci`, `npm --prefix web run typecheck` and
`npm --prefix web test`.

Workspace lints forbid `unsafe` code and deny `unwrap`, `expect` and `panic` outside tests; `#[allow]`,
`#[expect]`, `// @ts-*` comments and `eslint-disable` do not appear in source.

Edition 2024 makes `std::env::set_var` an `unsafe fn`, so no code or test sets environment variables in-process.
Every configuration parser is a pure function over an injected lookup (`impl Fn(&str) -> Option<String>`):
binaries pass `std::env::var`, tests pass a map, and tests of the command line set variables on the child process
(`assert_cmd`'s `.env()`).

The core crates name no agent host; `scripts/check-host-names.sh` enforces it. `cargo bench -p dibs` measures a hook
on a warm cache, process start included; CI reports it and does not gate on it. A pull request from a fork is
checked locally by a maintainer (`scripts/check-clean.sh` and a read of the diff) before it is merged.

## License

MIT. See [LICENSE](LICENSE).
