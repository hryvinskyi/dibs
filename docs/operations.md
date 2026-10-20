# Running the server

The server is one process on one SQLite file. It speaks plain HTTP on port 8080 and expects a proxy in front of it to
terminate TLS. This page covers the image, the compose example behind Caddy in both TLS modes, running behind a CDN,
configuration, members joining and leaving, how long records are kept, snapshots and restore, upgrades, the lock file
and the GHCR package.

## The image

Releases publish `ghcr.io/hryvinskyi/dibs:<version>` (and `latest` for a stable release). The image holds one binary,
`/dibs-server`, which is its entrypoint, so a subcommand is whatever follows the image name:

```sh
docker run --rm ghcr.io/hryvinskyi/dibs:latest --version
docker run --rm ghcr.io/hryvinskyi/dibs:latest restore /snapshots/<snapshot> --to /data/dibs.db
```

- It runs as the unprivileged user `65532`, from a minimal base with no shell.
- `/data` holds the database and `/snapshots` the snapshots; both are volumes. A named volume starts out writable by the
  user. A bind-mounted directory is not: hand it over once with `chown 65532:65532 <dir>`.
- The image sets `DIBS_DATABASE=/data/dibs.db` and `DIBS_SNAPSHOT_DIR=/snapshots`, so snapshots are on by default.
- It has no shell, so it carries no `HEALTHCHECK`. Probe `GET /health` from your orchestrator or monitor: it answers
  `{"ok":true}` without a token and reads nothing.
- To run a command in a running container, give the binary's full path:
  `docker compose exec dibs /dibs-server admin snapshot`.

Build it yourself from the repository root, which is the build context:

```sh
docker build -f deploy/Dockerfile -t dibs-server .
```

## Deploying with the compose example

`deploy/compose.example.yml` runs two services on a private network: `dibs`, with no published port, and `caddy`, which
terminates TLS and is the only thing the host publishes. The compose project is named `dibs`, so it never depends on
the directory's name and does not collide with another stack on the host.

```sh
mkdir dibs && cd dibs
# the files of deploy/, from a clone of the repository at the tag of the release you run
cp compose.example.yml compose.yml
cp Caddyfile.example Caddyfile
cp .env.example .env              # edit DIBS_HOST at least
mkdir -p data snapshots certs
sudo chown 65532:65532 data snapshots
cp /path/to/cert.pem /path/to/key.pem certs/
docker compose up -d
```

Create the first project and its admin. The command prints the admin's token alone on standard output, once:

```sh
docker compose exec dibs /dibs-server admin init --project widgets --admin alice --name "Alice Example"
```

Hand the token over through a password manager, never chat or email. Further members:
`docker compose exec dibs /dibs-server admin member add bob --project widgets --name "Bob Example"`.

The compose file pins the subnet of the `edge` network to `172.31.250.0/24`. `DIBS_TRUSTED_PROXIES` in `.env` names
that subnet, which is how the server knows that the address Caddy forwards is to be believed. Change both together if
the subnet collides with one of your own.

Pin `DIBS_VERSION` in `.env` to a release instead of `latest`, and pin the Caddy image by digest in a deployment you
keep. Caddy runs with a read-only filesystem, no Docker socket and no admin API. Because the admin API is off, a changed
Caddyfile takes effect with `docker compose restart caddy`.

### TLS from certificate files (the default)

`deploy/Caddyfile.example` serves `{$DIBS_HOST}:{$DIBS_PORT}` with the pair mounted read-only at
`/certs/cert.pem` and `/certs/key.pem`, and turns Caddy's automatic HTTPS off. It fits a CDN's origin certificate, a
corporate CA's certificate or any pair managed outside Caddy. Replace the files and restart Caddy when the pair is
renewed. `DIBS_PORT` is the port Caddy listens on, inside the container and on the host (443 by default).

### Caddy's automatic HTTPS

For a host with a public name that Caddy can prove it owns, `deploy/Caddyfile.auto.example` lets Caddy obtain and renew
the certificate itself. Use it as `Caddyfile` and change the compose file in three places:

1. Publish ports `80:80` and `443:443` on the `caddy` service (the challenge arrives on 80 or 443).
2. Replace the `/data` tmpfs of the `caddy` service with a persistent volume (`caddy-data:/data`, declared under
   `volumes:`). That is where Caddy keeps its certificates and account; on a tmpfs it would ask for new ones at every
   start and meet the certificate authority's rate limits.
3. Drop the `./certs:/certs:ro` mount.

No DNS-provider plugin and no provider credentials are involved: this mode needs the host reachable on 80 or 443.

### Behind a CDN

When a CDN sits between the visitors and the server, the address Caddy sees is the CDN's. The server limits failed
sign-ins per client address, so it needs the visitor's address, and it must take it from a header only when the peer is
a proxy it trusts.

- `deploy/Caddyfile.cdn.example` makes Caddy believe the CDN's client-address header only from the CDN's own address
  ranges (`trusted_proxies static …`, `client_ip_headers …`), and writes the address it resolved into a header for the
  server (`header_up X-Edge-Client-IP {client_ip}`). A peer that reached the port without passing the CDN therefore
  cannot forge it: Caddy overwrites the header. Put the ranges your CDN publishes in place of the example ones, and the
  header your CDN uses (a name such as `CF-Connecting-IP` is typical) in place of `X-Edge-Client-IP`.
- In `.env` set `DIBS_TRUST_PROXY=1`, `DIBS_TRUSTED_PROXIES` to the `edge` subnet and `DIBS_CLIENT_IP_HEADER` to the
  header the Caddyfile writes.
- **Firewall.** The published port should admit the CDN's ranges alone. Traffic to a port Docker
  publishes is forwarded to the container, not delivered to the host, so the rules a host firewall such as `ufw` keeps
  for incoming traffic never see it and a `ufw deny` does not close the port. Put the restriction in the `DOCKER-USER` chain, which Docker consults before its own
  rules, or bind the published port to an address a firewall in front of the host controls.
- **Event stream.** The stream sends a comment line (`: keep-alive`) every 25 seconds and answers with
  `Cache-Control: no-cache, no-transform` and `X-Accel-Buffering: no`. The CDN must not buffer, cache or compress
  `/v1/events/stream` or `/ui/events/stream`, and its idle timeout for a connection must exceed 25 seconds. Caddy is
  configured with `flush_interval -1` and no `encode` directive for the same reason.
- **Challenges.** A CDN's bot or WAF challenge answers non-browser clients with an HTML page. The `dibs` command line,
  its hooks and the MCP server are non-browser clients; exempt `/v1/*` and `/health` from challenges, or the clients
  report a server that answers nonsense. The pages under `/ui` are for browsers and can stay challenged.

## Configuration

The server reads its configuration from environment variables once, when it starts. An empty value counts as not set.
A value the server refuses stops it with status 2 and a sentence naming the variable.

| Variable | Default | Meaning |
|---|---|---|
| `DIBS_DATABASE` | none; required | The path of the SQLite database file. Created by `admin init` when it does not exist. |
| `DIBS_LISTEN` | `0.0.0.0:8080` | The address and port to listen on. |
| `DIBS_TRUST_PROXY` | `0` | `1` to take the client's address from a proxy; needs `DIBS_TRUSTED_PROXIES`. |
| `DIBS_TRUSTED_PROXIES` | none | The proxies' addresses, comma-separated, each an IP or a CIDR block (`172.31.250.0/24`). |
| `DIBS_CLIENT_IP_HEADER` | `X-Forwarded-For` | The header a trusted proxy names the client in. |
| `DIBS_LOG` | `info` | The most detailed level logged: `error`, `warn`, `info`, `debug`, `trace` or `off`. |
| `DIBS_SNAPSHOT_DIR` | none (snapshots off) | The directory snapshots are written into. It must exist. |
| `DIBS_SNAPSHOT_EVERY` | `6h` | The time between snapshots: a whole number and `s`, `m`, `h` or `d`, from `1m` to `30d`. |
| `DIBS_SNAPSHOT_KEEP` | `28` | How many of the newest snapshots are kept, 1 to 10000. |
| `DIBS_MIN_CLIENT` | none (every client served) | The oldest `dibs` client served, a release version such as `0.4.0`; older ones are answered `426`. See [Client versions](#client-versions). |
| `DIBS_LATEST_CLIENT` | none (no update recommended) | The `dibs` release clients are told to update to; not older than `DIBS_MIN_CLIENT`. |

`DIBS_SNAPSHOT_EVERY` and `DIBS_SNAPSHOT_KEEP` without `DIBS_SNAPSHOT_DIR` are refused. With `X-Forwarded-For` the
client is the right-most address outside the trusted set; any other header is read as one address. A malformed header
from a trusted proxy falls back to the proxy's address and logs a warning.

The server logs JSON lines to standard output. The request log names the method and the path, never a header, so a
token in `Authorization` or a cookie is not written anywhere. Every command other than `serve` writes its answer to
standard output and its log to standard error.

## Client versions

Every `dibs` client names its version in its `User-Agent` (`dibs/0.4.2`). Two settings decide what the server does with
it:

- **`DIBS_LATEST_CLIENT`** is the release the team should run. At the start of an agent's session, at most once a day
  and in the background, each client asks `GET /v1/version`; when this release is newer than its own, it says so in the
  session's brief (`[DIBS] update dibs: 0.4.1 → 0.4.2`) and installs it the way `dibs self-update` does, checked
  against the release key built into it. On Windows the background only tells, since a download there would hold
  the hook that started the sync; the person runs `dibs self-update`. A client that cannot install it (a directory
  its user cannot write to, a build without a release key) keeps running and keeps saying so.
- **`DIBS_MIN_CLIENT`** is the oldest client served. An older one is answered `426 client_too_old` with a sentence
  saying how to update. Its hooks read that as a server that cannot be reached: no decision, one warning a minute,
  and every edit goes through, so an old client never stops anyone's work; it is simply no longer coordinated. Its
  daily check still works, because `GET /v1/version` is never refused.

The order of an upgrade:

1. Upgrade the server first; it serves the clients it served before.
2. Once the new release's client binaries are published, set `DIBS_LATEST_CLIENT` to it and restart the server. The
   clients update themselves over the following day, as each person starts a session.
3. Raise `DIBS_MIN_CLIENT` only after `DIBS_LATEST_CLIENT` has been out for a week, so that a laptop that was closed for
   a few days has had its chance to update.

`dibs` also checks that the project's plugin is not newer than the binary (the plugin's `min_binary`): a plugin that
auto-updated ahead of the binary prints the update hint once per session and its hooks still exit 0.

## The release signing key

The client binaries are not code-signed, so `dibs self-update` takes their origin from a signature instead: each
release carries `SHA256SUMS.minisig`, a [minisign](https://jedisct1.github.io/minisign/) signature of its `SHA256SUMS`
made with the project's ed25519 release key. The public half is `keys/release.pub`, compiled into every binary; the
secret half is the repository secret `DIBS_RELEASE_SIGNING_KEY`, which the release workflow signs with. The release
workflow also writes the public key into the released `install.sh`, which checks the same signature when minisign is
installed on the machine and says it checked the checksum only when it is not. A build whose
`keys/release.pub` holds the placeholder instead of a key refuses every update and points to the install script, and
the release workflow refuses to publish a release whose signature does not verify against `keys/release.pub`.

[CONTRIBUTING.md](../CONTRIBUTING.md#release-signing) has the steps to make the key and to replace it.

## Commands

```text
dibs-server [serve]                                  serve until SIGTERM or Ctrl-C (the default)
dibs-server admin init --project <slug> --admin <handle> --name <name> [--login <login>]
dibs-server admin member add <handle> --project <slug> --name <name> [--github <login>] [--role admin|member|importer]
dibs-server admin member remove <handle> --project <slug>
dibs-server admin token rotate <project> <handle>
dibs-server admin thread delete <id> --project <slug>
dibs-server admin snapshot
dibs-server restore <snapshot> --to <database>
dibs-server migrate
```

`--github` is another name of `--login`: the member's login in the outside tracker, which an integration maps to them.

The commands that issue a token (`admin init`, `admin member add`, `admin token rotate`) print the token alone on
standard output and a sentence about it on standard error, so `token=$(… 2>/dev/null)` captures exactly the token. The
token is shown once; the server keeps only a hash. Exit status: 0 done, 1 refused or failed, 2 configuration refused.

Members can also be added, and their tokens rotated, with `dibs admin member add` and `dibs admin token rotate` from a
client signed in as an admin. `--role` is `member` by default; `importer` makes the token of an integration worker that
feeds the project's items from an outside tracker (`PUT /v1/items`): it may read members, items, the event log and the
ready list, and nothing else — no claim, no conversation.

## Members joining and leaving

**Joining.** `dibs admin member add <handle> --name <name> [--github <login>] [--role member|admin]` (from a client
signed in as an admin) or `dibs-server admin member add` (on the server) prints the new member's token once. Hand it
over through a password manager, never chat or email. The member stores it with `dibs login`, in the credentials file
readable by them alone (`0600` on macOS and Linux, an access list granting only them on Windows), or with
`dibs login --keychain` in the operating system's credential store. The handle `dibs` is taken: it is the name what
dibs writes itself goes by.

**Leaving.** `dibs admin member remove <handle>` (or `dibs-server admin member remove <handle> --project <slug>`) does
all of this in one transaction, so it happens whole or not at all:

- every token of the member is revoked and every browser session of theirs ends; their machines answer
  `dibs login` from then on, and their hooks fall silent with one warning;
- each of their claims is released and its item is open again (`force_released`, naming the admin, or `null` for the
  operator);
- each hold of theirs on a slot ends, their places in slot queues and the offers to them go, and a freed slot is offered
  to the next member waiting (`slot_force_released`, cause `member_removed`);
- they are marked removed (`member_removed`) and leave `GET /v1/members`;
- dibs tells the project, in one message from dibs addressed to everyone, what was released (the first 20 of each
  kind by name, then how many more, however much they held).

A request of theirs that was signed in before the removal is decided before it or refused after it: every request
checks, in its own transaction, that its member is still in the project.

Their messages, and every record that names them, stay; their handle stays taken. An admin cannot remove themself;
another admin or the operator can. A member removed by mistake is added again under another handle.

### Rotating a token

Rotate on an event: someone leaves, a token leaks, a laptop is lost. Tokens have no expiry.

```sh
docker compose exec dibs /dibs-server admin token rotate widgets bob
```

This revokes every token of the member, ends the browser sessions those tokens opened, and prints one new token. The
member signs in again with `dibs login`.

## How long records are kept

A background task removes what is past its date once when the server starts and then once a day, each project in a
transaction of its own; it stops with the server, finishing a pass it is running, before the write-ahead log is
checkpointed.

| Record | Kept |
|---|---|
| A conversation and its messages | 180 days after its last message; while it is about a work item that is still open (not done, not dropped), as long as the item is; while it carries an escalation no arbiter has settled, until one does |
| An event of the log | 90 days |
| A browser session | 12 hours after it was last used |
| A snapshot | the newest `DIBS_SNAPSHOT_KEEP`, and of those none older than 30 days, but the newest always |

Each pass that removed anything appends one `pruned` event with the counts (`threads`, `messages`, `events`) to the
project's log. A reader whose cursor is behind the events removed — an integration that was off for longer than the log
keeps — is answered `410 cursor_expired` (`details.pruned_through`): it reads the state again, then follows the log from
the start (`after=0`). An admin deletes a conversation at once with `dibs admin thread delete <id>` (a secret pasted by
mistake): its messages go, and the log keeps a `thread_deleted` event with its id alone.

## Snapshots

With `DIBS_SNAPSHOT_DIR` set, a background task writes a consistent copy of the database (`VACUUM INTO`) every
`DIBS_SNAPSHOT_EVERY`, starting as the server starts, and keeps the newest `DIBS_SNAPSHOT_KEEP`, of which it deletes
those older than 30 days too. The newest snapshot is never deleted, however old it is.

- A snapshot is named `dibs-<UTC timestamp>.db`, such as `dibs-20261019T215526.072Z.db`, so the names sort by age. It
  is written under a hidden temporary name, flushed to disk and renamed, so a file with a final name is whole. A crash
  in the middle can leave a hidden `.dibs-<UTC timestamp>.db.partial` file; the next snapshot removes such a file once
  it is more than an hour old.
- Only files of that name form are counted or deleted; the directory may hold other files.
- `dibs-server admin snapshot` writes one now (and applies the retention). The server logs each snapshot's size and
  duration.
- Snapshots on the same disk as the database protect against a mistake, not against losing the machine. Copy the
  directory off the host on a schedule. Do not copy the live `dibs.db`: it is in write-ahead-log mode, and a file copied
  while the server runs may miss what is still in the `-wal` file.

## Slots

A project declares its slots — the shared actions one member does at a time — in its settings, beside its areas, and
applies them with `dibs admin settings apply <file>`; `docs/api.md` lists every field. A slot's command patterns live
only there, never in a checkout's `.dibs.toml`:

```toml
[[slots]]
name = "deploy:staging"
max_hold = "30m"
group = "staging"
commands = [{ matches = '^deploy\.sh staging( |$)' }, { matches = '^make deploy', unless = '--dry-run' }]

[[slots]]
name = "db:staging"
max_hold = "1h"
group = "staging"
run_only = true
freezes = ["refs/heads/main"]
commands = [{ matches = '^migrate( |$)' }]
note = "a migration runs inside dibs slot run"
```

- **Prove the patterns before applying them.** `dibs slot test settings.toml -- '<command line>'` prints the simple
  commands a line splits into and the slot it needs, without a server; run it over a table of the real command lines.
- **`max_hold`** ends a hold its member forgot. While somebody waits, a holder can renew only up to twice `max_hold`
  from the take.
- **`group`** makes slots exclude each other: a deploy and a migration of the same environment in one group are never
  held at once.
- **`run_only`** slots are done through `dibs slot run <slot> --purpose … -- <command>`, which keeps the hold alive with
  a heartbeat every 30 seconds and releases it with the command's exit status. To stop a run from a script, send
  `SIGTERM` to `dibs slot run`: it passes the signal to the command and records it as the release's exit. A run whose heartbeats stop for 2
  minutes is shown stalled, not freed; another member takes it over only with `dibs slot takeover <slot> --reason …`.
- **`freezes`** names branches nobody but the holder may push while the slot is held.
- Removing a slot from the settings ends its hold, its queue and its offer in the same change.

The server sweeps the slots every 30 seconds — expiries, stalls, lapsed offers, silent waiters, and the offer to the
next waiter — for every project with a hold, a waiter or an offer. The sweep stops when the server is told to stop,
after the requests in flight, and finishes a sweep it is running before the write-ahead log is checkpointed. An admin
ends any hold with `dibs admin slot force-release <slot> --reason …`; the holder is sent the reason.

## Pull requests and the integration

A team that works on branches and pull requests declares, per repository, the branches only some members may push to
and whether its pull requests get a status (`docs/api.md` lists the fields). For a project with several repositories
each entry of `repos` carries its own; a project with one writes them at the top level:

```toml
[[protected_refs]]
ref = "refs/heads/main"
pushers = ["alice"]

[pr_status]
enabled = true
context = "dibs"
describe_holder = false
```

- **Protected branches** are what keeps the agents to the rule where the forge's plan has no branch protection or
  required reviews: a push there by anyone not in `pushers` is refused by the agent's command check and by
  `dibs check --push`, and so is a push whose branches cannot be read. Each push is judged against the repository of
  the remote it goes to; a fork protects nothing. Install the pre-push hook so that it passes git's arguments on:
  `exec dibs check --push --remote "$1" --url "$2"`. It judges protected branches from what the machine remembers, so
  a push to one is refused while the server cannot be reached. Nothing on the server can stop a person who pushes with
  `git push --no-verify`, or a merge through the forge's web pages; the rule is the team's agreement.
- **A claim lives through review.** A member records their pull request on their claim (`dibs claim review <item> --pr
  <owner>/<repo>#<number>`); the claim keeps its areas and never goes stale while in review. Its review ends when the
  integration reports the merge or the close of a pull request it recorded, or when its holder removes one recorded by
  mistake (`--drop-pr`); and the claim itself still ends as any claim does — its holder releases or completes it, an
  admin forces its release, or the member is removed. Only a merge into a protected branch (any branch, in a repository
  that protects none) completes it.
- **An integration closes the loop.** dibs speaks no forge API. A worker holding an `importer` token
  (`dibs admin member add worker --name "Integration" --role importer`) watches the forge and calls
  `GET /v1/review-claims`, `POST /v1/prs/merged`, `POST /v1/prs/closed` and `GET /v1/pr-status`, then writes the status
  on each open pull request's head commit — only when its state or description changed since it last wrote one for that
  commit, since a forge keeps a bounded number of statuses per commit and counts each write against its limits. On
  **every pass** it reconciles every pull request `GET /v1/review-claims` lists — asking the forge where each stands and
  reporting those already merged or closed — not only the pull requests that changed recently: a pull request merged
  before its holder recorded it, or while the worker was down, is reported that way and no other. Every report is
  idempotent: a repeat answers `matched: false`.
- **A silent integration is shown** where the project relies on one: while a claim is in review or a repository's
  `pr_status` is enabled. dibs records an `importer`'s calls, at most one a minute; when none has called for 15
  minutes, `GET /v1/status`, the session brief, `dibs status` and the board say since when. Merges then complete no
  claims and statuses go stale until it calls again: check the worker. A project that relies on no integration shows
  nothing.
- **`describe_holder`** decides whether a status names the slot, its holder and their purpose. Everyone who can read the
  repository reads a status; keep it `false` where they should not see who holds what.

## Branches and overlaps

dibs tells two members when their branches meet, before the second merge does: both change one file, one changes a
published contract the other's work depends on, or a merge into the base changed files a branch still changes on an old
base. Every claim records the branch its holder's checkout is on (the hook and the heartbeat send it); the rest comes
from the integration and the settings.

- **Declare what is published.** Per repository (at the top level for a project with one), `contracts` lists the paths
  other work depends on and who consumes them, and `area_deps` which areas depend on which:

  ```toml
  [[contracts]]
  paths = ["packages/*/src/Api/**"]

  [[contracts]]
  paths = ["schema/**"]
  consumers = ["web:*"]

  [area_deps]
  search = ["billing"]
  ```

  `area_deps` may be generated from the packages' own manifests. A change takes effect at once: the stored footprints
  are read again under the new settings, and the contract overlaps that makes are announced.
- **Report the footprints.** On every pass the integration reads the branches of the live claims with one call,
  `GET /v1/claims`, asks the forge what each branch changes against its base, and sends it with
  `PUT /v1/items/{id}/footprint`. For GitHub, that is `compare/{base}...{branch}` (conditional on its ETag), and the
  report's fields come from its answer: `paths` from `files[].filename` (at most 3000, `truncated: true` when the forge
  capped the list), `base` the base branch compared against, `base_sha` from `merge_base_commit.sha` and `base_at` from
  `merge_base_commit.commit.committer.date` — the merge base and its time, never the base's tip: that is what tells a
  branch that has a merge from one that does not. A repeat changes nothing, and a branch the claim no longer records
  answers `stored: false`. A branch deleted after its merge sends nothing; a footprint goes when its claim ends, its
  branch changes, or its branch is the one a delivering merge brought in. Send the merged pull request's `files` and
  its `head` branch with `POST /v1/prs/merged`, too: without `files` the merged branch's footprint stands for what
  moved, and without `head` the claim that recorded the pull request is taken to have been on the merged branch.
- **What it costs.** A footprint's paths are kept one row each, under an index by path, so the claims sharing a path
  are found by looking up the reported paths, not by reading every footprint; the contract overlaps are derived from a
  small summary of each footprint (its areas and its contract paths). A settings change reads every footprint once.
- **What people see.** Each overlap is announced once in a message from dibs to the holders it concerns (`high` for a
  shared file or a contract, `normal` for a moved base, which every merge brings), and listed by `GET /v1/status`,
  `dibs status` and the board while its claims are live. A claim going stale ends no overlap and is not told again when
  it comes back; an overlap ends when a footprint, a branch or the settings it rests on change, when a merge brings its
  branch in, or when a claim it names ends. Removing a repository from the settings drops its footprints and what they
  made.
- **Branches the client cannot read.** The hook and the heartbeat read the branch from git's files without starting git:
  `HEAD` of the directory `GIT_DIR` names, or of the nearest `.git` (a linked worktree's `.git` file is followed). A
  repository that keeps its refs in a reftable has a `HEAD` that names no branch (`refs/heads/.invalid`), so its claims
  record no branch and get no footprint.

## Restoring

A restore is offline and never replaces a database: it writes a new file. It refuses while a server holds the target
(see the lock file below), and refuses a target, or a `-wal` or `-shm` file by its name, that exists.

```sh
docker compose stop dibs
mkdir -p data/old && mv data/dibs.db* data/old/
docker compose run --rm dibs restore /snapshots/dibs-20261019T215526.072Z.db --to /data/dibs.db
docker compose up -d dibs
```

The snapshot is copied under a hidden name beside the target, opened there and brought up to the current schema, and
only then renamed to the target, so the target appears whole and migrated or not at all. The snapshot is never written
to. To keep the old database in place instead, restore to another path and point `DIBS_DATABASE` at it in `.env`.

## Upgrading

1. Read the release's notes.
2. Take a snapshot: `docker compose exec dibs /dibs-server admin snapshot`.
3. Set `DIBS_VERSION` in `.env` to the new release, then `docker compose pull && docker compose up -d`.

The schema is migrated by the server when it opens the database, so a start on a new release migrates the file.
Migrations only go forward: a server refuses a database that a newer release has migrated. To go back, restore the
snapshot taken before the upgrade onto the older release. `dibs-server migrate` migrates a database file without
serving, offline.

### Two spellings of one ticket

From the release that keeps `gh` references in lower case, a database whose items include two references that differ
only in case (`gh:Acme/Web#3` and `gh:acme/web#3`), or a `gh` reference that is not `owner/repo#number`, is refused by
the migration, which names the references; nothing is changed and the server does not start. The two items are two
records of one ticket, and only a person can tell which one carries the work. Merge them by hand on the stopped server,
after a snapshot:

```sh
sqlite3 dibs.db "SELECT project_id, id, external_ref, status FROM items
                 WHERE lower(external_ref) = lower('gh:acme/web#3');"
```

Keep one id (`<keep>`), the one with the claim or the conversations, and move what names the other (`<drop>`) onto it,
in one transaction:

```sql
BEGIN;
UPDATE threads SET item_id = <keep> WHERE project_id = <p> AND item_id = <drop>;
UPDATE grants SET item_id = <keep> WHERE project_id = <p> AND item_id = <drop>;
DELETE FROM shared_paths WHERE project_id = <p> AND item_id = <drop>;
DELETE FROM claims WHERE project_id = <p> AND item_id = <drop>;
DELETE FROM items WHERE project_id = <p> AND id = <drop>;
COMMIT;
```

A reference of another shape is corrected in place (`UPDATE items SET external_ref = 'gh:acme/web#3' WHERE …`) or
given a scheme of its own (`note:…`). Then start the server again; the migration lowers the remaining `gh` references.
The event log keeps the dropped id where it already named it.

Upgrade the server before the clients, then move the clients with `DIBS_LATEST_CLIENT` and `DIBS_MIN_CLIENT` (see
[Client versions](#client-versions)). A stop is graceful on `SIGTERM`: the server stops accepting connections, ends the
event streams, gives requests in flight 10 seconds, finishes a snapshot it is writing, checkpoints the write-ahead log
and closes the database. The compose file allows 20 seconds.

## The lock file

A server holds `<database>.lock` (`/data/dibs.db.lock`) for as long as it runs, as an exclusive advisory lock on that
file. `restore` and `migrate` take the same lock while they run, so they refuse a database a server is serving, and a
second server refuses it too: *another process holds the database*. The operating system releases the lock when the
process ends, however it ends, so a crashed server leaves nothing to clean up. The lock file itself stays and is reused;
do not delete it while a process runs. The `admin` commands only read and write rows through SQLite and do not take it,
which is why they work against a running server.

Keep the database on a local disk. Locks and SQLite's write-ahead log are unreliable on network filesystems.

## The GHCR package

The first push creates `ghcr.io/hryvinskyi/dibs` as a **private** package, and it stays private until its visibility is
set: until then `docker pull` fails for everyone not logged in to `ghcr.io` with a token that may read packages. After
the first release, open the package on GitHub (the repository's *Packages* section, then *Package settings*) and change
its visibility to public. The image carries the `org.opencontainers.image.source` label, which connects the package to
the repository.

## Troubleshooting

| Symptom | Cause |
|---|---|
| The server exits at start with *the database … could not be opened* | The `/data` directory is not writable by user `65532`. |
| *another server, a restore or a migration holds the database* | Another process has the database; stop it. |
| A client reports a `429` with `too_many_attempts` | Ten failed sign-ins from one address within a minute. If every client shares one address, the proxy's, `DIBS_TRUST_PROXY` and `DIBS_TRUSTED_PROXIES` are not set as above. |
| A client reports `503` with `too_many_streams` | A member holds 8 event streams open, or the server 128. |
| The board says *Integration silent since …* | Claims wait in review, or pull requests get a status, and no `importer` token has called for 15 minutes: the worker is down, its token was revoked, or it cannot reach the server. Merges complete no claims until it calls again. |
| An integration reports `410` with `cursor_expired` | It was off for longer than the log keeps events (90 days); it reads the state again and follows the log from `after=0`. |
| The web pages cannot sign in | The session cookie is `Secure`: use the pages over HTTPS (or `localhost`). |
