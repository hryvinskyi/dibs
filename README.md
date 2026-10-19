# dibs

dibs is a small coordination service for teams in which people and AI coding agents work on the same repositories.
Members claim the files and areas they are about to change, so that overlapping edits are caught before they are
written instead of at merge time. Agents and people also talk through threaded messages, from a web UI, a command
line, an MCP server and a guard that hooks into the agent hosts.

> Status: early development. The interfaces may still change before a first stable release.

## Install

### The command line

`dibs` is one binary for macOS, Linux and Windows. The install scripts pick the archive for your machine, check it
against the release's `SHA256SUMS`, and refuse to install anything that does not match.

macOS and Linux, into `~/.local/bin`:

```sh
curl -fsSL https://github.com/hryvinskyi/dibs/releases/latest/download/install.sh | sh
```

A particular release: `… | sh -s -- --version 1.2.3`. Set `DIBS_INSTALL_DIR` to install somewhere else.

Windows, in PowerShell, into `%LOCALAPPDATA%\Programs\dibs` (which the script adds to your user `PATH`):

```powershell
irm https://github.com/hryvinskyi/dibs/releases/latest/download/install.ps1 | iex
```

A particular release: set `$env:DIBS_VERSION = '1.2.3'` first.

Each release has an archive per platform and a `SHA256SUMS` file listing them:

| Platform | Archive |
|---|---|
| macOS, Apple silicon | `dibs-<version>-aarch64-apple-darwin.tar.gz` |
| macOS, Intel | `dibs-<version>-x86_64-apple-darwin.tar.gz` |
| Linux, x86-64 | `dibs-<version>-x86_64-unknown-linux-gnu.tar.gz` |
| Linux, arm64 | `dibs-<version>-aarch64-unknown-linux-gnu.tar.gz` |
| Windows, x86-64 | `dibs-<version>-x86_64-pc-windows-msvc.zip` |
| Windows, arm64 | `dibs-<version>-aarch64-pc-windows-msvc.zip` |

To install by hand, download the archive and `SHA256SUMS` from the release page, check the archive
(`sha256sum -c --ignore-missing SHA256SUMS`, `shasum -a 256 -c` on macOS, or compare `Get-FileHash` on Windows), and
put `dibs` (`dibs.exe`) on your `PATH`. The Linux binaries are built on Ubuntu 22.04 and need its glibc (2.35) or newer; they
do not run on musl systems such as Alpine. To build from source: `cargo build --release -p dibs`.

**The binaries are unsigned.** There is no Apple Developer ID and no Windows code-signing certificate behind them.

- On macOS, a file downloaded through a browser carries a quarantine attribute and Gatekeeper refuses to run it. Clear
  it once: `xattr -d com.apple.quarantine <path to dibs>`. The install script and `curl` set no such attribute, so a
  binary they install runs as it is.
- On Windows, a file downloaded through a browser carries the mark of the web and SmartScreen warns about it. Clear it
  once: `Unblock-File <path to dibs.exe>`. The install script does this for the file it installs.

### The server

The server is a container image, `ghcr.io/hryvinskyi/dibs`, from the same releases. See
[Running the server](#running-the-server).

## Quick start

**A server, to try it out** (plain HTTP on the loopback address only; [docs/operations.md](docs/operations.md) has
the real deployment):

```sh
docker run -d --name dibs -p 127.0.0.1:8080:8080 -v dibs-data:/data ghcr.io/hryvinskyi/dibs:latest
docker exec dibs /dibs-server admin init --project widgets --admin alice --name "Alice Example"
```

The second command prints the admin's token once, on its own line. Keep it.

**A client**, in a checkout of the repository the project is about:

```sh
# 1. Tell the checkout which server and project it belongs to. Commit this file.
cat > .dibs.toml <<'TOML'
server = "http://127.0.0.1:8080"
project = "widgets"
TOML

# 2. Sign in. The token is read from standard input and stored for the project.
dibs login

# 3. Name the project's areas (once, as an admin). See "Configuring a client" for the file.
dibs admin settings apply settings.toml

# 4. Claim the work you are about to do, with the areas you will change.
dibs new gh:acme/widgets#42 --title "Refund rounding"
dibs claim gh:acme/widgets#42 --area billing

# 5. Ask before you edit; a teammate who tries the same area is told who holds it.
dibs check packages/billing/rounding.rs
```

Add a teammate with `dibs admin member add bob --name "Bob Example"`, which prints their token once; hand it over
through a password manager. With the Claude Code plugin, step 5 happens by itself before every edit (see
[Agent hosts](#agent-hosts)).

## Concepts

- **Project.** One team's workspace on a server. It has members, settings, work items and a log of events. A token
  belongs to one member of one project and sees nothing outside it.
- **Member.** A person with a handle (`alice`) and a role: `admin` (also manages members, tokens and settings),
  `member`, or `importer`, a bot that feeds items in from an outside tracker and takes part in nothing else.
- **Work item.** A unit of work, usually named by a reference into an outside tracker, written `scheme:value`
  (`gh:acme/widgets#42`), with a title and a status: `open`, `claimed`, `done` or `dropped`.
- **Area.** A named set of paths in a repository (`billing`: `packages/billing/**`), written `repo:area` when a project
  has several repositories. Areas, not individual files, are what members hold.
- **Claim.** A member's hold on a work item and on the areas they will change. A claim is *live* while its session is
  seen (a heartbeat, an edit, a renewal) within the project's `stale_after`, 24 hours by default, and *stale* after that.
  Only one member holds an area at a time. A stale claim can be taken over, with a reason that is sent to its holder.
- **Edit policy.** With `require-claim` (the default), a member may edit a governed path only under a live claim that
  covers it; with `advise` they may, and are told who else is in the area. A path no area or rule governs is free.
- **Hot-file rules.** Policies for files many people touch, matched by glob: `regenerate` (anyone may change it, nobody
  merges it by hand), `owner` (only the holder of a named area), `warn-shared` (anyone, with the other editors named) and
  `free`.
- **Grant.** A holder lets another member edit inside an area they hold, for that member's own live claim.
- **Thread.** A conversation between members, or with the whole project, optionally about a work item. Agents and people
  use the same threads.
- **Session.** One running client of a member: a terminal, an agent. Claims are kept alive by their session's
  heartbeats.
- **Event.** Everything above appends to the project's log, which the web pages and clients follow live.

## Running the server

The server is `dibs-server`, one process on one SQLite file, shipped as the image `ghcr.io/hryvinskyi/dibs`. It listens
on plain HTTP and expects a proxy to terminate TLS. [deploy/compose.example.yml](deploy/compose.example.yml) runs it
behind Caddy, which serves a certificate and key you mount (or obtains its own certificate), and
[docs/operations.md](docs/operations.md) covers that, a CDN in front, every setting, snapshots and restore, upgrades and
the lock file. The settings, each an environment variable:

| Variable | Default | Meaning |
|---|---|---|
| `DIBS_DATABASE` | required (`/data/dibs.db` in the image) | The database file. |
| `DIBS_LISTEN` | `0.0.0.0:8080` | Address and port. |
| `DIBS_TRUST_PROXY`, `DIBS_TRUSTED_PROXIES`, `DIBS_CLIENT_IP_HEADER` | off | Take the client's address from a trusted proxy. |
| `DIBS_LOG` | `info` | Log level. |
| `DIBS_SNAPSHOT_DIR`, `DIBS_SNAPSHOT_EVERY`, `DIBS_SNAPSHOT_KEEP` | off; `6h`; `28` | Where snapshots go, how often, how many are kept. |

The commands: `dibs-server serve` (the default), `admin init | member add | token rotate | snapshot`,
`restore <snapshot> --to <database>` and `migrate`. Probe `GET /health` for liveness.

## Configuring a client

**`.dibs.toml`** sits at the root of a checkout and is committed. The nearest one wins, and the search stops at the
first directory that holds a `.git`, so a clone nested in another answers for itself.

```toml
server = "https://dibs.example.com"   # https, except for a loopback address
project = "widgets"
repo = "api"                          # only in a project that lists several repositories
```

With `repo`, the checkout's `origin` must be the remote the project's settings give that repository; a file copied into
another clone then governs nothing.

**Project settings** are a TOML file that an admin applies with `dibs admin settings apply settings.toml`. A project
with one repository declares its areas and rules at the top level:

```toml
stale_after = "24h"             # how long a claim stays live without a sign of life (1h to 30d)
heartbeat_every = "1m"          # how often a session sends one (1m to 15m)
edit_policy = "require-claim"   # or "advise"

[[areas]]
name = "billing"
paths = ["packages/billing/**"]

[[areas]]
name = "search"
paths = ["packages/search/**"]

[[hot]]
paths = ["**/*.lock"]
kind = "regenerate"
note = "Regenerate it; do not merge it by hand."

[[hot]]
paths = ["config/billing.yml"]
kind = "owner"
area = "billing"
```

A project with several repositories lists them, each with its own areas and rules; the two layouts are never mixed:

```toml
[[repos]]
id = "api"
remote = "git@example.com:acme/api.git"

[[repos.areas]]
name = "billing"
paths = ["src/billing/**"]

[[repos]]
id = "web"
remote = "git@example.com:acme/web.git"

[[repos.areas]]
name = "checkout"
paths = ["src/checkout/**"]
```

Globs are matched against the path from the repository root: `*` and `?` stay inside a directory, `**` crosses
directories, `[…]` and `{a,b}` work as in a shell, and case matters. The first area that contains a path governs it.

**Credentials.** `dibs login` reads a token from standard input, checks it with the server and stores it for the
project. The token is found, in order, in the `DIBS_TOKEN` variable, then in the credentials file, then (if you signed
in with `--keychain`) in the operating system's credential store.

| | Credentials file | State (caches, sessions) |
|---|---|---|
| macOS | `~/.config/dibs/credentials.toml` | `~/Library/Application Support/dibs` |
| Linux | `$XDG_CONFIG_HOME/dibs/credentials.toml`, else `~/.config/dibs/…` | `$XDG_STATE_HOME/dibs`, else `~/.local/state/dibs` |
| Windows | `%APPDATA%\dibs\credentials.toml` | `%LOCALAPPDATA%\dibs` |

`DIBS_CONFIG_DIR` and `DIBS_STATE_DIR` move them. On macOS and Linux the credentials file must be readable by its user
alone (mode `0600`) and is refused otherwise; on Windows it is written with an ACL that grants only the current user.
`dibs login --keychain` keeps the token in the macOS Keychain, the Linux Secret Service or the Windows Credential
Manager instead. `DIBS_OFF=1` switches the guard off on a machine.

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

Every command takes `--json` to print its answer as JSON.

```sh
dibs login [--server <url>] [--keychain]          # sign in; the token comes from standard input
dibs whoami                                       # who the stored token signs in as, and where
dibs status                                       # live and stale claims and the areas held
dibs items [--mine] [--status open|claimed|done|dropped]
dibs new gh:acme/widgets#42 --title "Refund rounding"      # create an item, or show the one that exists
dibs claim gh:acme/widgets#42 --area billing --area search # claim, or renew your claim
dibs takeover 12 --reason "alice is out; I am picking this up" --area billing
dibs release 12                                   # give the item back
dibs done 12                                      # the item is finished
dibs check packages/billing/a.rs                  # may I edit these paths?
dibs check --git-range origin/main..HEAD [--author me]   # warn about what a range of commits changed
dibs grant 14 --area api:billing --to bob         # let bob edit in an area you hold, for his item 14
dibs send --to alice --subject "Rounding" "May I touch round.rs?"    # or --project; "-" reads stdin
dibs inbox [--unread]                             # conversations waiting for you
dibs thread 31                                    # read one
dibs read 31                                      # mark it read
dibs sync [--force] [--quiet]                     # heartbeat and refresh what this machine remembers
dibs mcp                                          # the same operations as MCP tools on stdin and stdout
dibs hook <host> <event>                          # answer an agent host's hook
dibs doctor                                       # the agent hosts this build supports
dibs admin member add bob --name "Bob Example" [--login bob-gh] [--admin]
dibs admin token rotate bob                       # revoke bob's tokens and print a new one
dibs admin settings apply settings.toml
dibs admin force-release 12                       # end anyone's claim on item 12
```

An item is named by its local id (`12`) or its reference (`gh:acme/widgets#42`).

## HTTP API

Every route, request, response and error code is in [docs/api.md](docs/api.md). In short: JSON under `/v1`, a bearer
token in the `Authorization` header, errors as `{ "error": { "code", "message" } }` with a stable `code`, and the
project's event log as a page (`GET /v1/events`) or as server-sent events (`GET /v1/events/stream`).

## Web UI

The server also serves pages for people at `/ui`, where you sign in with your token:

- **Status** (`/ui/`): live claims, stale claims with a take-over form, and the areas held.
- **Board** (`/ui/board`): what is ready to pick up, what is in progress, what went stale and what was done today,
  filterable by area and member, with a banner when something changes.
- **Conversations** (`/ui/threads`): read the conversations between members and agents, and reply.

The pages are rendered on the server and enhanced by a few scripts of its own; they follow the event log live. The
session cookie is `Secure`, so use them over HTTPS (or on `localhost`).

## Security

- **Tokens.** Every member has their own token, shown once when it is issued; the server stores only a hash. Hand a
  token over through a password manager, never chat or email. Rotate on events (someone leaves, a token leaks, a laptop
  is lost) with `dibs admin token rotate`. A token has no expiry.
- **Tenancy.** A token belongs to one member of one project, and every query is scoped to that project: a record of
  another project is reported as missing, the same as one that does not exist. A conversation is visible to its
  participants only. Roles are `admin`, `member` and `importer`; an importer takes part in no conversation and holds
  no claim.
- **Sign-in attempts** are limited per client address (ten failures a minute), and a client that is refused is told how
  long to wait.
- **TLS** is the proxy's job. The server speaks plain HTTP, and the client refuses a plain `http` server address that
  is not a loopback address, so a token never crosses a network unencrypted. The pages' cookie is `HttpOnly`, `Secure`
  and `SameSite=Strict`, and their content security policy allows nothing but the server's own scripts and styles.
- **Logs** name the method and the path of a request and never a header.
- **What the edit hook can and cannot see.** The hook of an agent host sees the file edits the host's own tools make,
  before they are written. It does not see edits made through a shell command (`sed -i`, a redirect, a formatter), a
  rename or a move, a file a code generator writes, or the changes a merge, rebase or checkout brings in. It is a
  guard against two agents editing the same area, not a lock on the files. `dibs check --git-range origin/main..HEAD`
  is its complement: it looks at every path the commits of a range changed (merge commits left out, both sides of a
  rename) and warns about those someone else holds or that need a claim. It always succeeds, so a git `pre-push` hook
  can call it without ever stopping a push.
- **Unsigned binaries.** See [Install](#install) for what that means on macOS and Windows.
- **Reporting a vulnerability.** Use the repository's private vulnerability reporting (the *Security* tab, then *Report
  a vulnerability*) rather than a public issue.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the build, the gates and the rules a change follows.

## License

MIT. See [LICENSE](LICENSE).
