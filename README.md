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

| Host | Status | What its guard sees |
|---|---|---|
| Claude Code | supported, through the `dibs` plugin | session start, each file edit before it is written, every tool after it ran; edits another member holds are refused, and subagents are told apart |
| Codex CLI | planned | its command hooks |
| OpenCode | planned | its in-process plugins, through a shim over `dibs hook neutral` |

`dibs doctor` shows the hosts this binary supports and what each can carry. `DIBS_OFF=1` switches the guard off on a
machine: every hook answers without objection and contacts nobody.

### Claude Code

The plugin gives Claude Code the brief at the start of a session, a check before every `Edit`, `Write`,
`MultiEdit` and `NotebookEdit`, heartbeats after tools run, the `dibs_*` tools of `dibs mcp`, and a skill with the
workflow. It needs the `dibs` command on the `PATH` and a `dibs login` for the project; without them its hooks say
so and let everything through.

1. Install `dibs` and sign in: `dibs login --server <url>` inside a checkout that has a `.dibs.toml`.
2. Add this repository as a plugin marketplace and install the plugin, from a shell:

   ```sh
   claude plugin marketplace add hryvinskyi/dibs
   claude plugin install dibs@dibs
   ```

   or inside a session: `/plugin install dibs --marketplace hryvinskyi/dibs`.
3. Start a new session in the checkout. `dibs doctor` shows whether the plugin is installed.

The hooks never approve a tool call: a call dibs has no objection to goes through Claude Code's own permission
settings as it would without the plugin. A refusal names the holder and the way on. When the server cannot be
reached, the hooks say so once a minute and let edits through.

On Windows, Claude Code runs a plugin's hooks with Git Bash, so the plugin needs
[Git for Windows](https://git-scm.com/downloads/win). Without it the hooks cannot start, and they are declared so
that a hook that cannot start never blocks a call; the MCP tools work either way.

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
