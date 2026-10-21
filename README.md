# dibs

dibs is a small coordination service for teams in which people and AI coding agents work on the same repositories.
Members claim the files and areas they are about to change, so that overlapping edits are caught before they are
written instead of at merge time. Agents and people also talk through threaded messages, from a web UI, a command
line, an MCP server and a guard that hooks into the agent hosts.

> Status: early development. The interfaces may still change before a first stable release.

## Install

### The command line

`dibs` is one binary for macOS, Linux and Windows. The install scripts pick the archive for your machine, check it
against the release's `SHA256SUMS`, and refuse to install anything that does not match. When [minisign](https://jedisct1.github.io/minisign/)
is installed, `install.sh` also checks the release's signature of `SHA256SUMS` against the release key written into
it, and refuses a signature that does not verify; without minisign it says that it checked the checksum only.
`install.ps1` checks the checksum only and says so.

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

### Updating

`dibs self-update` replaces the installed binary with the newest release (`--version 1.2.3` for a particular one). It
never installs an older release than the one installed. Before anything is written it checks the release's
`SHA256SUMS` against the signature the release carries (`SHA256SUMS.minisig`, a minisign ed25519 signature made with
the project's release key, whose public half is built into `dibs`), and the archive against `SHA256SUMS`; a missing or
wrong signature, or an archive that does not match, installs nothing. The new binary is written beside the old one and
put in its place in one step. On Windows a running `dibs.exe` cannot be overwritten, so it is renamed to
`dibs.old.exe` first and deleted the next time `dibs` starts.

It needs to write into the directory `dibs` is installed in. When that directory is not yours (a system directory, a
package manager's), it refuses and tells you to install the new release with the install script into a directory you
own. Like the install script, it downloads from a process, so macOS sets no quarantine attribute on what it installs.

You rarely need to run it. When the team's server recommends a newer release (`DIBS_LATEST_CLIENT`), the start of an
agent's session says so in its brief (`[DIBS] update dibs: 0.4.1 → 0.4.2 — run dibs self-update in a terminal`) and
the background sync installs it, at most once a day, never inside a hook's own time. On Windows the background only
tells: run `dibs self-update` there yourself. A server can also stop serving clients older than a version
(`DIBS_MIN_CLIENT`): such a client's hooks give one warning a minute and let every edit through, and its commands say
how to update. [docs/operations.md](docs/operations.md#client-versions) has the rules for operators.

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

Add a teammate with `dibs admin member add bob --name "Bob Example" --github bob-gh`, which prints their token once;
hand it over through a password manager, never chat or email. When someone leaves, `dibs admin member remove bob`
revokes their tokens and releases everything they held at once. With the Claude Code plugin, step 5 happens by itself before every edit (see
[Agent hosts](#agent-hosts)).

## Concepts

- **Project.** One team's workspace on a server. It has members, settings, work items and a log of events. A token
  belongs to one member of one project and sees nothing outside it.
- **Member.** A person with a handle (`alice`) and a role: `admin` (also manages members, tokens and settings),
  `member`, or `importer`, a bot that feeds items in from an outside tracker and takes part in nothing else.
- **Work item.** A unit of work, usually named by a reference into an outside tracker, written `scheme:value`
  (`gh:acme/widgets#42`), with a title and a status: `open`, `claimed`, `done` or `dropped`. A `gh` reference is kept in
  lower case, so every spelling of one ticket names one item. An importer also reports a priority (0 is soonest, 4
  latest), the areas the work will probably touch, the tracker's own state, and whether the tracker closed it.
- **Dependency.** "This item is blocked by that one", recorded by a person (`dibs dep add`) or reported by the importer.
  An item is *blocked* while one of its blockers is neither done nor closed. A cycle is refused. An import replaces only
  the edges it reported, never a person's.
- **Ready work.** The open items you can pick up now: nobody holds them live, nothing unfinished blocks them, and no
  other member holds an area they will probably touch. Soonest first; every other open item comes with why it waits.
- **Area.** A named set of paths in a repository (`billing`: `packages/billing/**`), written `repo:area` when a project
  has several repositories. Areas, not individual files, are what members hold.
- **Claim.** A member's hold on a work item and on the areas they will change. A claim is *live* while its session is
  seen (a heartbeat, an edit, a renewal) within the project's `stale_after`, 24 hours by default, and *stale* after that.
  Only one member holds an area at a time. A stale claim can be taken over, with a reason that is sent to its holder.
- **Feature and debt work.** A claim is a feature (the default) or debt work (`--debt`: refactoring, clean-up), and
  keeps that kind. A feature claim that declares an area other members hold only for debt work is not refused: the area
  waits on it, the debt holder is asked, urgently and in one conversation per pair of claims, to hand it over once their
  person agrees (`dibs claim narrow`), and it moves to the feature claim in the transaction that lets the last holder
  go — a narrowing, the debt claim's end, or its going stale. A debt claim in review is asked once it is worked on
  again; a request unanswered for the project's `yield_escalates_after` (30 minutes by default) goes to the arbiters.
  Two features keep the plain refusal, and an area waits for one claim at a time. The board marks debt work and what
  each claim waits for.
- **Review.** Work reaches a protected branch through pull requests. A member records the pull request of their claim
  (`dibs claim review`); the claim is then *in review*: it keeps its areas, so nobody edits the files under review, and
  never goes stale while it waits. The merge of its last recorded pull request completes it, reported by an integration
  that watches the forge; with `--more`, the merge returns it to work for the next pull request of the item. A close, or a
  merge into a branch that is not protected, delivers nothing and returns the claim to its holder. Branch names and how
  a pull request names its ticket are a team's own conventions; the agents' workflow suggests defaults (`bob/42-slug`,
  `Refs #42`) that many teams use.
- **Overlap.** Each claim records the branch its holder's checkout is on, and an integration reports what each branch
  changes against its base (its *footprint*). When two members' branches change one file, when one changes a published
  contract (`contracts`) that the other's area depends on (`area_deps`), or when a merge into the base changes files a
  branch still changes on its old base, dibs tells the holders once, in a message that says to rebase now — and, for a
  contract, to read the other change first. Two claims of one member never collide; a moved base is told to anyone
  whose branch is behind, the merge's own author included.
- **Protected branch.** A branch of a repository only the members a project lists may push to; everyone else's agent is
  refused a push there, with the pull request as the way on, and so is a push whose branches dibs cannot read. A push to
  a fork is not judged. The pre-push check judges protection from what the machine remembers, so it holds while the
  server cannot be reached. Where the forge offers no branch protection, this is how the agents keep that rule;
  `git push --no-verify` breaks it. A rule names a branch exactly (`refs/heads/main`) or by a prefix ending in `*`
  (`refs/heads/release/*`, every release branch), and a refusal for a pattern names it. Tags are protected the same
  way, by name or by a prefix ending in `*` (`refs/tags/v*`), by the agents' command check and the pre-push check
  alike; a tag's refusal names no way past it. A tag rule holds only for clients of 0.1.1 or newer, and a branch
  pattern only for clients of 0.1.2 or newer: an older one is served the settings without what it cannot read.
- **Edit policy.** With `require-claim` (the default), a member may edit a governed path only under a live claim that
  covers it; with `advise` they may, and are told who else is in the area. A path no area or rule governs is free.
- **Hot-file rules.** Policies for files many people touch, matched by glob: `regenerate` (anyone may change it, nobody
  merges it by hand), `owner` (only the holder of a named area), `warn-shared` (anyone, with the other editors named) and
  `free`.
- **Grant.** A holder lets another member edit inside an area they hold, for that member's own live claim.
- **Wide claim.** A claim announced over many areas for a change that cuts across them (an upgrade, a rename). It holds
  none of them, so it is in nobody's way. Each member holding one of its areas is sent an announcement that asks to be
  acknowledged, and acknowledging it is their grant: until then the wide claim's edits in their area are refused. One
  area is in one live wide claim at a time, and whoever claims an area inside one later is asked too.
- **Thread.** A conversation between members, or with the whole project, optionally about a work item. Agents and people
  use the same threads.
- **Messages from dibs.** What dibs writes itself — a wide claim's announcements, a request to hand an area over to a
  feature and the notice that it was handed over, the notice that a claim or a slot hold
  was taken over, ended, expired or that an offer lapsed, an escalation, a member leaving — is authored by the project,
  shown as *from dibs* everywhere and never counted toward anyone's limit on raised messages.
- **Arbiters and escalation.** The members a project names (`arbiters` in its settings) to settle what agents do not
  agree on. `dibs escalate` sends an urgent message, from dibs, to the arbiters and to whoever holds and whoever asked for
  the item, slot or conversation in question; the board shows it until an arbiter acknowledges it. Agents do not settle
  priority between people: they escalate and pick other work meanwhile.
- **Importance and acknowledgement.** A message is `normal`, `high` or `urgent`, and may ask each recipient to
  acknowledge it. An acknowledgement is the recipient's own record that they read it; the message never changes, and its
  author sees who has acknowledged and who has not. An acknowledgement says "I read this"; it authorises nothing, except
  that acknowledging a wide claim's announcement grants its member edits in your areas it names.
- **Slot.** A named shared action one member does at a time — a deploy, a release, a migration. A claim says what
  someone works on; a slot says who may act now. A slot hold is a member's time-boxed right to act (`max_hold`), with a
  queue of waiters: when it ends, the first waiter is offered the slot for a few minutes, and anyone else who takes it
  meanwhile waits behind them. Slots of one group exclude each other. A hold is kept by the member's heartbeats, by the
  30-second heartbeats of `dibs slot run` (and shown *stalled*, not freed, when they stop), or until a forge workflow
  run finishes. A held slot can freeze branches nobody else may push meanwhile.
- **Session.** One running client of a member: a terminal, an agent. Claims are kept alive by their session's
  heartbeats.
- **Event.** Everything above appends to the project's log, which the web pages and clients follow live.
- **Retention.** A conversation is kept 180 days after its last message (as long as its work item is open, if it is
  about one), an event 90 days, a snapshot 30 days; see [operations](docs/operations.md#how-long-records-are-kept).

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
project. At a terminal it reads one line without showing it; where the line cannot be read hidden (no controlling
terminal) it says so on standard error and reads it visibly. Piped in, it reads all of standard input. On Windows, in
Git Bash's mintty or another terminal without a console, use `dibs login < token.txt`: the token is then never shown,
and the read never waits on a console that is not there. An admin replaces their own token with
`dibs admin token rotate <your-handle> | dibs login`: into a pipe, the admin commands that issue a token print the
token alone and their sentence on standard error. Pipe only your own token into your login; another member's would sign
you in as them. If the login half fails after the rotation (the server cannot be reached, the pipe breaks), the new
token is lost and the old one is revoked already: rotate again on the server with
`dibs-server admin token rotate <project> <handle>`. The token is found, in order, in the `DIBS_TOKEN` variable, then in the credentials file, then (if you signed
in with `--keychain`) in the operating system's credential store.

| | Credentials file | State (caches, sessions) |
|---|---|---|
| macOS | `~/.config/dibs/credentials.toml` | `~/Library/Application Support/dibs` |
| Linux | `$XDG_CONFIG_HOME/dibs/credentials.toml`, else `~/.config/dibs/…` | `$XDG_STATE_HOME/dibs`, else `~/.local/state/dibs` |
| Windows | `%APPDATA%\dibs\credentials.toml` | `%LOCALAPPDATA%\dibs` |

`DIBS_CONFIG_DIR` and `DIBS_STATE_DIR` move them. On macOS and Linux the credentials file must be readable by its user
alone (mode `0600`) and is refused otherwise; on Windows it is written with an ACL that grants only the current user.
`dibs login --keychain` keeps the token in the macOS Keychain, the Linux Secret Service or the Windows Credential
Manager instead. A hook never waits on the credential store: it reads it with a one-second deadline, and a store that
asks the person to allow access, or answers late, counts as no token — the hook decides nothing and warns once, naming
`dibs login`. The release binaries are not code-signed, and macOS ties a Keychain item to the binary that wrote it, so
after each `dibs self-update` macOS may ask once to allow the new binary access: run `dibs whoami` in a terminal and
allow it there, never from inside an agent's session. `DIBS_OFF=1` switches the guard off on a machine.

## Agent hosts

Every agent host reaches dibs through MCP (`dibs mcp`, the same for all of them) and through its hooks, which call
`dibs hook <host> <event>`. The guard behind the hooks is one for every host; each host has a small adapter between its
hook format and the guard's. [docs/hosts.md](docs/hosts.md) has the neutral protocol, the capabilities and how to add
a host.

| Host | Status | What its guard sees |
|---|---|---|
| Claude Code | supported, through the `dibs` plugin | session start, each file edit before it is written, each `Bash` command before it runs, every tool after it ran, each prompt; edits another member holds and commands that need a slot another member holds are refused, a teammate's pressing message or a slot offered to you arrives between tool calls (or, opted in, wakes a session that waits for you), and subagents are told apart |
| Codex CLI | planned | its command hooks |
| OpenCode | planned | its in-process plugins, through a shim over `dibs hook neutral` |

`dibs doctor` shows the hosts this binary supports and what each can carry. `DIBS_OFF=1` switches the guard off on a
machine: every hook answers without objection and contacts nobody, and `dibs check --push` lets every push go. The
switch is not hidden: the machine counts each session that ran with it on, and the next report shows it to the
project's admins ([docs/operations.md](docs/operations.md#a-pilots-figures)).

### Claude Code

The plugin gives Claude Code the brief at the start of a session, a check before every `Edit`, `Write`,
`MultiEdit` and `NotebookEdit`, a check of every `Bash` command against the project's slots, heartbeats after tools
run, a teammate's `high` or `urgent` message (or one that asks for an acknowledgement) and a slot offered to you between
tool calls and with your prompt, the `dibs_*` tools of `dibs mcp`, and a skill with the workflow. It needs the `dibs` command on the `PATH` and a `dibs login` for the project; without them its hooks say
so and let everything through.

1. Install `dibs` and sign in: `dibs login --server <url>` inside a checkout that has a `.dibs.toml`.
2. Add this repository as a plugin marketplace and install the plugin, from a shell:

   ```sh
   claude plugin marketplace add hryvinskyi/dibs
   claude plugin install dibs@dibs
   ```

   or inside a session: `/plugin install dibs --marketplace hryvinskyi/dibs`.
3. Turn on auto-update for the `dibs` marketplace, so the plugin follows the releases: in a session, run `/plugin`, open
   the **Marketplaces** tab, select `dibs` and choose **Enable auto-update**. Claude Code turns auto-update off by
   default for a third-party marketplace. A team can turn it on for everyone who opens a repository in its committed
   `.claude/settings.json`, which registers the marketplace and its auto-update together
   (`extraKnownMarketplaces`, with `"autoUpdate": true` beside its `source`):

   ```json
   {
     "extraKnownMarketplaces": {
       "dibs": { "source": { "source": "github", "repo": "hryvinskyi/dibs" }, "autoUpdate": true }
     },
     "enabledPlugins": { "dibs@dibs": true }
   }
   ```

   Claude Code then refreshes the marketplace in the background after a session starts and updates the plugin on
   disk; the next session loads the new version. `DISABLE_AUTOUPDATER=1` or `DISABLE_UPDATES=1` on a machine turns this
   off unless `FORCE_AUTOUPDATE_PLUGINS=1` is also set. (Claude Code's plugin documentation:
   [Keep plugins updated](https://code.claude.com/docs/en/discover-plugins#keep-plugins-updated),
   [When auto-update runs](https://code.claude.com/docs/en/plugins/loading#when-auto-update-runs),
   [`extraKnownMarketplaces`](https://code.claude.com/docs/en/settings-reference#extraknownmarketplaces).)
4. Start a new session in the checkout. `dibs doctor` shows whether the plugin is installed.

A plugin that auto-updated can be newer than the `dibs` binary on the machine. Its hooks never block anything because
of that: at the start of a session it compares the version it needs with `dibs --version` and, when the binary is
older, says once to run `dibs self-update`; every hook exits 0 whatever the binary answers.

The hooks never approve a tool call: a call dibs has no objection to goes through Claude Code's own permission
settings as it would without the plugin. A refusal names the holder and the way on. When the server cannot be
reached, the hooks say so once a minute and let edits and commands through.

A `Bash` command is split into simple commands and judged on your machine, from the settings dibs keeps there; the
command line never reaches the server, which is asked only where the slots the command touches stand. A message
between tool calls reaches only the main agent, never a subagent, at most once a minute, and once across all your
sessions: the first session to claim it shows it. It is framed as a teammate's request that authorises nothing.

#### Waking a session that waits for you (opt-in)

Between tool calls is as soon as the hooks can deliver: a session that waits for your next prompt hears nothing until
you type. Claude Code's [channels](https://code.claude.com/docs/en/channels) let the plugin's tool server push a
pressing message or a slot offer into the open session instead, which then starts a turn on it. It is off unless you
turn it on for a session, in two places at once:

```sh
DIBS_PUSH_TO_IDLE=1 claude --dangerously-load-development-channels plugin:dibs@dibs
```

- **Both are needed.** The flag makes Claude Code listen to the plugin's tool server; the variable makes the tool server
  push. Claude Code drops a push it is not listening for without telling anyone, so with the variable alone the
  pushes would be lost — and each one is claimed first, so the hooks would not show it either. Set the variable only
  on a command line that also passes the flag (an alias of the two is the easy way).
- **Research preview.** Channels are a research preview of Claude Code: the flags and the contract may change, neither
  flag appears in `claude --help`, and the development flag shows a warning at every start and works only in an
  interactive session (not with `-p`). They need Claude Code signed in with a claude.ai account or a Console API key;
  Amazon Bedrock, Google Cloud and Microsoft Foundry sessions get none. On a claude.ai Team or Enterprise plan an
  owner first turns channels on (`channelsEnabled`); an organisation can then list
  `{ "marketplace": "dibs", "plugin": "dibs" }` in `allowedChannelPlugins`, after which
  `claude --channels plugin:dibs@dibs` starts it without the warning. A Pro or Max account without an organisation
  needs the development flag.
- **What it pushes**, and when: only while the session waits for you (the plugin's hooks note when a turn ends), and
  only a `high` or `urgent` message or a slot offered to you — never a `normal` message, and never a wide claim's
  announcement, whose acknowledgement would grant edits — at most three wakes per sender in fifteen minutes. A busy
  session hears from its hooks between tool calls as before. A push is framed as the hooks frame a message, as a
  teammate's words quoted as data that authorise nothing, at the same rate (at most once a minute, never one thing
  twice, once across all your sessions), and it never asks the agent to acknowledge anything: that is yours to do.
  The tool server never offers to relay Claude Code's permission prompts, so nothing a teammate writes can approve a
  tool call. A session woken by a message says you are away, answers only when a decision is needed, never agrees or
  commits to anything for you, and goes on only with work you already asked for; a permission prompt waits for you.
- **Nothing is lost.** When nothing shows a push was seen within five minutes — the agent did not read it, you did not
  type — the session's hooks show it at the next tool call or prompt. When `DIBS_PUSH_TO_IDLE` is set, the brief at the
  start of a session reminds you that pushes need the flag above.
- **What it costs.** The tool server keeps one event stream open to the server for the session, and tells the server
  once a minute that it still runs.

On Windows, Claude Code runs a plugin's hooks with Git Bash, so the plugin needs
[Git for Windows](https://git-scm.com/downloads/win). Without it the hooks cannot start, and they are declared so
that a hook that cannot start never blocks a call; the MCP tools work either way.

## Command line

Every command takes `--json` to print its answer as JSON. A command you run gives each sending of a request 5 seconds
to connect and 20 seconds in all, so a request sent once more may take up to 40 seconds. It sends once more: a read (`GET`) whose connection timed out opening or was reset; and, only when its connection timed
out opening so that nothing reached the server, registering a session, applying the settings, acknowledging a message,
adding or removing a dependency, narrowing a claim, revoking a token, and creating an item named by a reference.
Nothing else is sent again, and nothing is sent again after a reset but a read, so that an answer never reports as
unchanged what a lost first sending changed. The hooks,
`dibs sync`, `dibs mcp` and the checks git's hooks run (`dibs check --push`, `dibs check --git-range`) keep their
limits (a hook's calls 2 seconds; the others 2 seconds to connect and 5 in all) and send nothing again.

```sh
dibs login [--server <url>] [--keychain]          # sign in; the token comes from standard input
dibs whoami                                       # who the stored token signs in as, and where
dibs status                                       # live and stale claims, the areas held, the wide claims
dibs items [--mine] [--status open|claimed|done|dropped]
dibs ready                                        # what you can pick up now, soonest first, and why the rest waits
dibs dep add gh:acme/widgets#42 --blocked-by gh:acme/widgets#40   # record a dependency; dep rm removes it
dibs new gh:acme/widgets#42 --title "Refund rounding"      # create an item, or show the one that exists
dibs claim gh:acme/widgets#42 --area billing --area search # claim, or renew your claim
dibs claim gh:acme/widgets#50 --wide billing,search --reason "Upgrade the HTTP client"   # a wide claim
dibs claim review gh:acme/widgets#42 --pr acme/widgets#7 [--more]   # your claim waits in review; the merge completes it
dibs claim review gh:acme/widgets#42 --drop-pr acme/widgets#8       # remove a pull request recorded by mistake
dibs claim gh:acme/widgets#7 --area billing --debt    # debt work: a feature claim may take its areas
dibs claim narrow gh:acme/widgets#7 --area billing    # give an area up; a feature claim waiting for it gets it
dibs takeover 12 --reason "alice is out; I am picking this up" --area billing [--debt|--feature]
dibs release 12                                   # give the item back
dibs done 12                                      # the item is finished
dibs check packages/billing/a.rs                  # may I edit these paths?
dibs check --git-range origin/main..HEAD [--author me]   # warn about what a range of commits changed
dibs check --push --remote "$1" --url "$2"       # git's pre-push hook: refuse a push to a protected branch or tag, or one a held slot freezes
dibs grant 14 --area api:billing --to bob         # let bob edit in an area you hold, for his item 14
dibs send --to alice --subject "Rounding" "May I touch round.rs?"    # or --project; "-" reads stdin
dibs send --project --subject "Freeze" --importance urgent --ack "No pushes to main until noon."
dibs inbox [--unread] [--unacked]                 # conversations waiting for you; what awaits your acknowledgement
dibs thread 31                                    # read one, with each message's id and who acknowledged it
dibs read 31                                      # mark it read
dibs ack 57                                       # acknowledge message 57 once you have read it (for a wide claim's announcement, that grants)
dibs escalate gh:acme/widgets#42 --reason "alice's agent and mine both need billing today"   # or --slot, --thread
dibs feedback false-refusal r-7f3a51 --reason "billing was released an hour ago"   # a refusal that was wrong, by its id
dibs feedback conflict src/billing/rates.rs --reason "rebased over a rounding change"   # a conflict resolved by hand
dibs slot take deploy:prod --purpose "ship 2.4" [--wait 9m] [--until-run deploy.yml@<sha>]   # exit 1 while queued
dibs slot run deploy:prod --purpose "ship 2.4" -- ./deploy.sh prod   # hold it while the command runs, then release
dibs slot release deploy:prod                     # the first member waiting is offered it
dibs slot leave deploy:prod                       # stop waiting, or give up the offer to you
dibs slot list                                    # every slot: who holds it and why, until when, who waits
dibs slot test settings.toml -- 'cd ops && ./deploy.sh prod'   # what a command line needs, without a server
dibs slot takeover deploy:prod --purpose "finish it" --reason "the deploy log stopped; checked the hosts"
dibs sync [--force] [--quiet] [--update] [--report]   # heartbeat and refresh what this machine remembers
dibs self-update [--version 1.2.3]                # install a newer release, checked against the release key
dibs mcp [--host <host>]                          # the same operations as MCP tools on stdin and stdout
dibs hook <host> <event>                          # answer an agent host's hook
dibs doctor                                       # the agent hosts this build supports
dibs admin member add bob --name "Bob Example" [--github bob-gh] [--role admin|member|importer]
dibs admin member remove bob                      # revoke bob's tokens, release his claims and slots; his messages stay
dibs admin token rotate bob                       # revoke bob's tokens and print a new one (alone, into a pipe)
dibs admin thread delete 31                       # delete a conversation at once (a secret pasted by mistake)
dibs admin settings apply settings.toml
dibs admin force-release 12                       # end anyone's claim on item 12
dibs admin slot force-release deploy:prod --reason "the release is cancelled"
```

An item is named by its local id (`12`) or its reference (`gh:acme/widgets#42`).

`dibs slot run` passes `SIGTERM` and `SIGHUP` to the command, waits for it, and records the signal as the release's
exit; it exits with the command's status. Ctrl-C reaches the command from the terminal: the wrapper does not send it
again, waits for the command and releases with the command's own outcome. To stop a run from a script, send `SIGTERM`
to `dibs slot run`. On Windows the console hands Ctrl-C and Ctrl-Break to the command itself. If the hold is taken over
while the command runs, it says so once, lets the command finish and releases nothing.

## HTTP API

Every route, request, response and error code is in [docs/api.md](docs/api.md). In short: JSON under `/v1`, a bearer
token in the `Authorization` header, errors as `{ "error": { "code", "message" } }` with a stable `code`, and the
project's event log as a page (`GET /v1/events`) or as server-sent events (`GET /v1/events/stream`). An integration
worker feeds the project from an outside tracker with an `importer` token: it sends its whole view to `PUT /v1/items` as
often as it likes, and a view equal to what is stored changes nothing. The same token reports merged and closed pull
requests (`POST /v1/prs/merged`, `POST /v1/prs/closed`), lists the claims in review (`GET /v1/review-claims`) and reads
the status to set on pull requests (`GET /v1/pr-status`), lists the live claims and their branches
(`GET /v1/claims`), and reports what each claim's branch changes (`PUT /v1/items/{id}/footprint`); each report may
be sent again. When no integration has called
for 15 minutes while claims wait in review or pull requests get a status, the status, the brief and the board say so.

## Web UI

The server also serves pages for people at `/ui`, where you sign in with your token:

- **Status** (`/ui/`): live claims, stale claims with a take-over form, the areas held, and the wide claims with who
  has acknowledged each of their areas.
- **Board** (`/ui/board`): a banner for each escalation of yours, or to you, that waits for an arbiter; the slots with
  who holds each and why, until when, a stalled run in red and who waits, the live wide claims with who has
  acknowledged them, your ready list with priorities, likely
  areas and links to the tracker, the open items that wait with their blockers, what is in progress and what waits in
  review with its pull requests, what went stale and what was done today, filterable by area and member, a banner
  for each overlap between live claims, and a banner while the project's integration is silent. The columns refresh by themselves when claims, items or slots change;
  while you are typing a take-over reason they stay as they are and a banner offers a reload instead.
- **Conversations** (`/ui/threads`): read the conversations between members and agents, and reply. Each conversation
  shows its importance and whether a message in it awaits your acknowledgement; a message that asks for one lists who has
  acknowledged it, updated as they do, and offers you an *Acknowledge* button while it still asks (an ended wide claim's
  announcement no longer does), and a reply can ask everyone to acknowledge it. What dibs wrote itself is marked *from
  dibs*.

The pages are rendered on the server and enhanced by a few scripts of its own; they follow the event log live. The
session cookie is `Secure`, so use them over HTTPS (or on `localhost`).

## Security

- **Tokens.** Every member has their own token, shown once when it is issued; the server stores only a hash. Hand a
  token over through a password manager, never chat or email. Rotate on events (a token leaks, a laptop is lost) with
  `dibs admin token rotate`; when someone leaves, `dibs admin member remove` revokes every token of theirs and releases
  what they held. A token has no expiry.
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
- **Pushes during a freeze.** A slot may freeze branches (`freezes`), by name or by a prefix ending in `*`
  (`api:refs/heads/release/*`): while another member holds it, nobody else pushes there. The agents' command check
  refuses such a `git push`, and `dibs check --push`, called from a git `pre-push` hook (`exec dibs check --push` in
  `.git/hooks/pre-push`), refuses it whoever typed it and whatever the command line said, since git hands it the exact
  branches. It is silent otherwise; a server that cannot be reached
  lets the push go with one warning, and `git push --no-verify` goes past it on purpose.
- **Raised messages are limited.** A member may send ten `high` or `urgent` messages an hour (`raised_per_hour` in the
  settings); the next is refused with `importance_limited` until the oldest is an hour old. What dibs sends itself is
  not counted.
- **Unsigned binaries.** See [Install](#install) for what that means on macOS and Windows.
- **Reporting a vulnerability.** Use the repository's private vulnerability reporting (the *Security* tab, then *Report
  a vulnerability*) rather than a public issue.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the build, the gates and the rules a change follows.

## License

MIT. See [LICENSE](LICENSE).
