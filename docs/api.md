# HTTP API

The server answers JSON under `/v1`, one health probe, and the pages of the web UI under `/ui`. The command line, the
MCP server and the hooks are clients of this API; the examples below use `curl` and a made-up project `widgets` with the
members `alice` and `bob`.

## Conventions

- **Format.** Requests with a body are JSON and carry `Content-Type: application/json`. Answers are JSON. Times are
  RFC 3339 in UTC (`2026-03-02T09:00:00Z`). Ids are positive whole numbers. Members are named by their handle. An area is
  written `repo:area` (`api:billing`); in a project with one repository the repository is called `default`. An external
  reference is `scheme:value` (`gh:acme/widgets#42`); a reference never appears in a path, only in a query string or a
  body, because it holds `/` and `#`. A `gh` reference is `gh:owner/repo#number` and is kept in lower case, since GitHub's
  names are case-insensitive: `gh:Acme/Widgets#42` and `gh:acme/widgets#42` are one reference, and a `gh` value of
  another shape is refused (`invalid_request`). A reference of another scheme is kept as written.
- **Caching.** Every answer carries `Cache-Control: no-store` unless its route sets its own.
- **Limits.** A request body is at most 128 KiB; an import batch (`PUT /v1/items`) at most 2 MiB and 500 items. A request is answered within 10 seconds or refused with `timeout`
  (the event stream is bounded only until its answer starts). A message body is at most 16 KiB.
- **Unknown fields.** A field a route does not read is ignored in a request, except in the settings document and in an
  import batch (`PUT /v1/items`), which refuse unknown fields.

## Authentication

Every route but `GET /health` and the pages needs a member's token:

```sh
curl -H "Authorization: Bearer $DIBS_TOKEN" https://dibs.example.com/v1/me
```

A token is `dibs_` followed by 32 to 128 letters, digits, `_` or `-`. It is issued once, by `dibs-server admin init`,
by `dibs-server admin member add` or `POST /v1/admin/members`, and rotated by `dibs-server admin token rotate` or
`POST /v1/admin/members/{handle}/tokens`. The server keeps only a hash. A token belongs to one member of one project,
and every request sees that project only: a record of another project is reported as missing.

A missing, malformed, unknown or revoked token is answered `401 unauthorized` with `WWW-Authenticate: Bearer`, the same
for every cause. Failed attempts are counted per client address (see `DIBS_TRUST_PROXY` in
[operations](operations.md)): ten in a minute, refilled evenly, after which the address is answered
`429 too_many_attempts` with a `Retry-After` header and `details.retry_after`, before its token is looked at.

Roles: `member` and `admin` are people; `importer` is an integration that feeds the project from an outside tracker. An
integration holds no claim, receives no message and reads no conversation. Its token may call exactly the routes marked
*any* and *import* below — `GET /v1/me`, `GET /v1/members`, `GET /v1/events`, `GET`, `POST` and `PUT /v1/items`,
`GET /v1/items/{id}` and `GET /v1/ready` — and every other route answers it `403 forbidden`. Routes marked *people*,
*import* and *admin* answer `403 forbidden` to a role they do not name before they read the request.

## Errors

Every failure has the same shape:

```json
{
  "error": {
    "code": "area_held",
    "message": "The area api:billing is held by another member.",
    "details": { "area": "api:billing", "holder": "alice", "item": 12, "item_ref": "gh:acme/widgets#7" }
  }
}
```

`code` is stable and written for programs; `message` is an English sentence for the person reading it and never carries
an internal error's text; `details` is present only when the refusal names something the caller can act on. A refusal
that names an item names it by its reference (`item_ref`, beside its id `item`) when it has one. The server logs the
cause of an internal failure and answers `internal`.

| Status | Codes |
|---|---|
| `400` | `invalid_json` (the body is not JSON of the shape the route expects), `invalid_query` |
| `401` | `unauthorized` |
| `403` | `forbidden`, `not_holder` (details: `holder`), `not_grantor` |
| `404` | `not_found` (also: a record of another project, and a conversation the caller may not read) |
| `405` | `method_not_allowed` |
| `409` | `held` (details: `holder`, `since`, `last_seen`), `stale` (`holder`, `last_seen`), `area_held` (`area`, `holder`, `item`, `item_ref`), `not_stale` (`holder`, `last_seen`), `nothing_to_take`, `own_claim`, `no_claim`, `item_closed` (`status`), `grantee_has_no_claim`, `handle_taken`, `project_exists`, `dependency_cycle` |
| `413` | `body_too_large` |
| `415` | `unsupported_media_type` |
| `422` | `invalid_request` (details: `field`), `unknown_area` (`area`), `invalid_area`, `ambiguous_area` (`candidates`), `unknown_repo` (`repo`), `unlisted_repository` (`remote`), `self_grant`, `not_a_recipient`, `no_recipients`, `invalid_settings`, `self_dependency`, `batch_too_large` (`max`), `invalid_batch`, `duplicate_ref` (`ref`) |
| `429` | `too_many_attempts` (details: `retry_after`) |
| `500` | `internal` |
| `503` | `timeout`, `too_many_streams` |

## Routes

*Auth* is `any` (every role), `people` (not an integration), `import` (an admin or the importer) or `admin`.

| Method and path | Auth | What it does |
|---|---|---|
| `GET /health` | none | The process answers: `{"ok":true}`. |
| `GET /v1/me` | any | The caller, their project and its settings. |
| `GET /v1/members` | any | The project's members. |
| `POST /v1/sessions` | people | Register a session, or update the one of the same label. |
| `POST /v1/heartbeat` | people | A session's sign of life. |
| `GET /v1/items` | any | List work items. |
| `POST /v1/items` | any | Create a work item, or return the one a reference names. |
| `PUT /v1/items` | import | Create and update the items an outside source lists, and their imported dependencies. |
| `GET /v1/items/{id}` | any | One item and its claim. |
| `POST /v1/items/{id}/dependencies` | people | Record that the item is blocked by another. |
| `DELETE /v1/items/{id}/dependencies` | people | Remove that the item is blocked by another. |
| `GET /v1/ready` | any | The work the caller can pick up now, and why the other open items wait. |
| `POST /v1/items/{id}/claim` | people | Claim an item, or renew your claim. |
| `POST /v1/items/{id}/takeover` | people | Take over another member's stale claim. |
| `POST /v1/items/{id}/release` | people | Release your claim. |
| `POST /v1/items/{id}/done` | people | Complete your claim. |
| `GET /v1/status` | people | Live claims, stale claims, the areas held and the caller's ready list. |
| `POST /v1/edit-check` | people | May I edit these paths? |
| `POST /v1/grants` | people | Let another member edit in an area you hold. |
| `DELETE /v1/grants/{id}` | people | Withdraw a grant you gave. |
| `POST /v1/threads` | people | Start a conversation. |
| `GET /v1/threads` | people | List the conversations you may read. |
| `GET /v1/threads/{id}` | people | A conversation with its messages. |
| `POST /v1/threads/{id}/messages` | people | Post a message. |
| `POST /v1/threads/{id}/read` | people | Mark a conversation read. |
| `GET /v1/inbox` | people | Conversations waiting for you. |
| `GET /v1/events` | any | A page of the project's event log. |
| `GET /v1/events/stream` | people | The event log as server-sent events. |
| `POST /v1/admin/members` | admin | Add a member. |
| `POST /v1/admin/members/{handle}/tokens` | admin | Rotate a member's token. |
| `DELETE /v1/admin/tokens/{prefix}` | admin | Revoke one token. |
| `PUT /v1/admin/settings` | admin | Replace the project's settings. |
| `POST /v1/admin/items/{id}/force-release` | admin | End any member's claim. |

A path that is no route is answered `404 not_found`; a route with the wrong method, `405 method_not_allowed`. A path
segment that is not an id names no record and is answered `404 not_found`.

### Records

```text
member   { handle, display_name, external_login | null, role: "admin" | "member" | "importer" }
session  { session_id, host, label, agent, push, last_seen }
item     { id, ref | null, title, status: "open" | "claimed" | "done" | "dropped",
           repo | null, unlisted_remote | null, priority: 0..4, areas_hint: ["repo:area", …],
           source_status | null, closed, source | null, created_by, created_at }
item ref { id, ref | null, title, link | null }
claim    { item, holder, session_id, host, areas: ["repo:area", …], claimed_at, renewed_at, last_seen }
grant    { id, item, area, from, to, created_at }
thread   { id, subject, item | null, author, to_project, to: [handle, …], created_at }
message  { id, thread, author, label, body, created_at }
summary  { thread, messages, unread, last_message_at | null }
```

`repo` of an item is the project's repository that its reference names, when the project lists it; `unlisted_remote` is
the repository the reference names when the project does not list it. `priority` (0 is soonest, 4 latest, 2 when nothing
said otherwise), `areas_hint` (the areas the work will probably touch; advisory, no claim declares them),
`source_status` (the outside tracker's own state, as it shows it), `closed` (the tracker closed the item; a claim on it
stays) and `source` (the importer's source that lists it) are what an importer reports. An *item ref* is how an answer
mentions another item: `link` is the tracker's page for its reference (a `gh` reference links to its GitHub issue), or
`null`.

### Me and members

`GET /v1/me`:

```json
{
  "member": { "handle": "alice", "display_name": "Alice Example", "external_login": null, "role": "admin" },
  "project": { "slug": "widgets" },
  "repos": [{ "id": "default", "remote": null }],
  "settings": { "stale_after": "1d", "heartbeat_every": "1m", "edit_policy": "require-claim" }
}
```

`settings` is the settings document in its normal form (see [Settings](#settings)); applying it again changes nothing.
`GET /v1/members` answers `{ "members": [member, …] }`, ordered by handle.

### Sessions

`POST /v1/sessions`, body `{ "host": "laptop", "label": "laptop/cli", "agent": "cli", "push": false }`; `host`, `label`
and `agent` are required, `push` (default `false`) says the session can be woken while idle. Answers `200` with the
session. A label registers once per member; registering it again updates the session and keeps its id.

`POST /v1/heartbeat`, body `{ "session_id": 7 }`, answers `{ "refreshed": [12] }`: the items whose claims the heartbeat
kept alive. A claim is *live* while its holder was seen within the project's `stale_after`; a heartbeat, an edit check
that reserves, or a renewal counts as seen.

### Items

`GET /v1/items` takes optional filters, all of which must match: `status` (`open`, `claimed`, `done`, `dropped`),
`holder` (a handle) and `ref` (an exact reference). It answers `{ "items": [item, …] }` ordered by id.

`POST /v1/items`, body `{ "ref": "gh:acme/widgets#42", "title": "Refund rounding" }` (`title` required, `ref`
optional; an item without a reference is local). Answers `201 { "item": …, "created": true }`, or `200` with
`"created": false` and the existing item when the reference already names one. In a project that lists its repositories, a
person's reference that names a repository the project does not list is refused with `unlisted_repository` (`422`, so
a typo never opens a second item beside the real one); an integration is not refused.

`GET /v1/items/{id}` answers `{ "item": item, "claim": claim | null }`; the claim may be live or stale.

`PUT /v1/items` is how an integration worker feeds the project from an outside tracker (an admin may call it too). Its
body is the whole view the worker read:

```json
{
  "source": "board",
  "complete": true,
  "items": [
    { "ref": "gh:acme/widgets#41", "title": "Refund rounding", "priority": 1, "areas_hint": ["billing"],
      "source_status": "Todo", "closed": false, "blocked_by": ["gh:acme/widgets#40"] },
    { "ref": "gh:acme/widgets#40", "title": "Rate table", "blocker": true }
  ]
}
```

`ref` and `title` are required; `priority` (default 2), `areas_hint`, `source_status`, `closed` (default `false`) and
`blocked_by` are optional. An item marked `"blocker": true` is carried only because a listed item waits for it: it gets
its title and `closed` from the batch, belongs to no source, and its own facts and edges are left as they are. Unknown
fields are refused. One batch reports at most 500 items, each reference once (`batch_too_large`, `duplicate_ref`).
The rules, applied in one transaction:

- An item is keyed by its reference: one the project does not have is created (an event `item_created`), one it has is
  updated only when a fact changed (`item_updated`). A batch equal to what is stored writes nothing and appends no
  event, so a worker may send its view as often as it likes.
- An area of `areas_hint` the project does not declare is left out and named in `dropped_hints`, never refused. A bare
  name is the item's own repository's area, else the one area of that name the project declares.
- A `closed` item the project does not know is not created (`skipped_closed`); a known one is marked closed. Closing
  never touches a claim.
- `blocked_by` replaces the item's *imported* edges; an edge a person added with `POST …/dependencies` stays. An edge to
  a reference neither the project nor the batch knows is refused on its own (`unknown_blocker`), and so is one that
  would make two items wait for each other (`dependency_cycle`) or an item wait for itself (`self_dependency`); the rest
  of the batch applies. A closed blocker the project does not know blocks nothing and is dropped.
- With `"complete": true` (the worker read every page without an error; it needs `source`), every open item of that
  source the batch did not mention is marked closed. A partial read never sends `complete`.
- Nothing in a batch claims, releases or completes anything.

It answers `200`:

```json
{
  "items": [
    { "ref": "gh:acme/widgets#41", "outcome": "created", "id": 52, "dropped_hints": [] },
    { "ref": "gh:acme/widgets#40", "outcome": "unchanged", "id": 51, "dropped_hints": [] }
  ],
  "closed": [47],
  "edges_added": 1,
  "edges_removed": 0,
  "refused_edges": [
    { "item": "gh:acme/widgets#41", "blocked_by": "gh:acme/api#9", "reason": "unknown_blocker",
      "message": "The blocker is neither a work item of the project nor in the batch." }
  ]
}
```

`outcome` is `created`, `updated`, `unchanged`, `skipped_closed` or `refused` (with `refusal: { code, message }`, an
admin naming a repository the project does not list); `dropped_hints` lists `{ area, reason }` with `reason`
`undeclared`, `ambiguous` or `malformed`; `closed` holds the ids a complete batch closed.

### Ready work and dependencies

`GET /v1/ready` answers the caller's ready list and, beside it, every other open item with why it waits:

```json
{
  "ready": [
    { "item": item, "link": "https://github.com/acme/widgets/issues/40", "stale_holder": null }
  ],
  "excluded": [
    { "item": item, "link": "https://github.com/acme/widgets/issues/41", "reasons": [
      { "reason": "blocked_by", "items": [item ref] },
      { "reason": "area_held", "area": "default:billing", "holder": "bob", "item": item ref }
    ] }
  ]
}
```

An item is weighed when it is open — not done, not dropped, not closed by its tracker. It is *ready* when no live claim
holds it, nothing it waits for is still unfinished (a blocker stops blocking when it is done or closed), and no area of
its `areas_hint` is held by another member's live claim. A stale claim does not hide an item: it is ready, with
`stale_holder` naming whose claim went stale (taking it is a takeover). Both lists are ordered by `priority`, then oldest
first. `reasons` are, in order: `claimed` (`holder`), `blocked_by` (`items`), `area_held` (`area`, `holder`, `item`).
What is held depends on who asks: an area the caller holds does not keep an item from them.

`POST /v1/items/{id}/dependencies`, body `{ "blocked_by": 51 }`, records that the item is blocked by `blocked_by` (both by
local id). The edge is the person's: an import never removes it. `DELETE /v1/items/{id}/dependencies?blocked_by=51`
removes the edge, whoever recorded it (an imported one comes back with the next import that reports it). Both answer
`{ "item": item, "blocked_by": item, "changed": true }`, with `changed: false` when the edge was already there or was not
there. Refusals: `dependency_cycle` (`409`, the blocker already waits for the item, directly or through others),
`self_dependency`, `not_found`, `invalid_request`.

### Claims

`POST /v1/items/{id}/claim`, body `{ "session_id": 7, "areas": ["billing"] }`. An area is written as a person writes
it: `repo:area`, or a bare name that exactly one repository declares. Answers

```json
{ "outcome": "new", "claim": { "item": 12, "holder": "alice", "areas": ["default:billing"], "…": "…" }, "dropped_areas": [],
  "blocked_by": [] }
```

`outcome` is `new` for a first claim and `renewed` when the caller already held the item (a stale claim of the caller's
included); a renewal adds the requested areas to those held and lists in `dropped_areas` those it no longer holds
because another member holds them now or the settings dropped them. Refusals: `held` (another member's live claim holds
the item), `stale` (their claim is stale: only a takeover may take it), `area_held` (another member's live claim holds
an area), `unknown_area`, `invalid_area`, `ambiguous_area`, `item_closed`. An area belongs to one member at a time. A
claim on a blocked item is made all the same — dependent work is often started in parallel — and `blocked_by` lists, as
item refs, the items it waits for that are neither done nor closed, for the client to warn about.

`POST /v1/items/{id}/takeover`, body `{ "session_id": 7, "reason": "…", "areas": [] }`, takes over another member's
stale claim and tells them the reason in a conversation about the item. Answers
`{ "claim": …, "previous_holder": "bob", "dropped_areas": [], "thread_id": 31 }`. Refusals: `not_stale` (the claim is
live), `own_claim`, `nothing_to_take`, `area_held`.

`POST /v1/items/{id}/release` and `POST /v1/items/{id}/done` take no body and answer `{ "claim": claim }`, the claim that
ended; the item is open again, or done. Refusals: `not_holder` (`403`), `no_claim` (`409`).

`GET /v1/status` answers

```json
{
  "live":  [{ "item": item, "claim": claim }],
  "stale": [{ "item": item, "claim": claim }],
  "holds": [{ "area": "default:billing", "holder": "alice", "item": 12 }],
  "ready": { "ready": [], "excluded": [] }
}
```

`ready` is the caller's ready list, as `GET /v1/ready` answers it.

### Edit check

`POST /v1/edit-check`, body `{ "session_id": 7, "repo": "default", "paths": ["packages/billing/a.rs"], "reserve": false }`.
Every path lies in the one repository `repo` names (an id from `/v1/me`) and is relative to its root, `/`-separated.
With `reserve: true` the edit is being made and what the check plans is recorded on the caller's claim; without it, the
check writes nothing. Answers a verdict per path, in the order asked:

```json
{ "verdicts": [
  { "repo": "default", "path": "packages/billing/a.rs", "verdict": "allowed" },
  { "repo": "default", "path": "packages/search/b.rs", "verdict": "needs_claim" },
  { "repo": "default", "path": "packages/billing/c.rs", "verdict": "held_by",
    "holder": "bob", "item": 14, "item_ref": "gh:acme/widgets#14", "area": "default:billing", "reason": "ask_holder" }
] }
```

| `verdict` | Meaning | Extra fields |
|---|---|---|
| `allowed` | The caller may edit. | |
| `allowed_with_note` | The caller may edit; a hot-file rule has something to say. | `rule`, `note`, `touched_by: [{ holder, item }]` |
| `needs_claim` | The path is governed, the policy requires a claim, and the caller holds none that covers it. | |
| `held_by` | Another member's live claim holds the path's area. | `holder`, `item`, `item_ref` (its reference, or `null`), `area`, `reason` (`ask_holder`: message them or ask for a grant) |
| `granted` | The holder granted the caller edits in the area. | `by`, `area` |

A path that no area or rule governs is `allowed`. Refusals: `unknown_repo` (`422`), `invalid_request` for a missing
field or a path that is not a relative path, `not_found` for a session that is not the caller's.

### Grants

`POST /v1/grants`, body `{ "item": 14, "area": "default:billing", "to": "bob" }`: the caller, who holds the area, lets
`to` edit in it, for `to`'s live claim on `item`. Answers `201 { "grant": grant, "created": true }`, or `200` with
`"created": false` when the same grant exists. Refusals: `not_holder` (`403`), `self_grant`, `grantee_has_no_claim`,
`unknown_area`, `not_found`. A grant ends when it is revoked, when its grantor no longer holds the area, or when the
grantee's claim ends.

`DELETE /v1/grants/{id}` answers `{ "grant": grant }`; only the grantor may (`not_grantor`, `403`).

### Conversations

`POST /v1/threads`, body

```json
{ "subject": "Billing rounding", "to": ["alice"], "item": 12, "label": "laptop/cli", "body": "May I touch round.rs?" }
```

`to` names members by handle; with `"to_project": true` instead the conversation goes to every person in the project;
naming both, or neither, is `invalid_request`. `item` (optional) is the work item it is about, `label` the session
label it is signed with. Answers `201 { "thread": thread, "message": message }`. Refusals: `not_found` for an unknown
recipient or item, `not_a_recipient` for an integration, `no_recipients` for an empty `to`.

`GET /v1/threads?item=12` answers `{ "threads": [summary, …] }`, the conversations the caller may read, newest activity
first. `GET /v1/inbox?unread=true` answers the same shape for the conversations waiting for the caller; with
`unread=true` (or `1`) only those with messages they have not read. A conversation the caller may not read is
reported as missing, the same as one that does not exist.

`GET /v1/threads/{id}` answers `{ "thread": thread, "messages": [message, …] }`, oldest first; reading does not mark it
read. `POST /v1/threads/{id}/messages`, body `{ "label": "laptop/cli", "body": "…" }`, answers `201 { "message": message }`.
`POST /v1/threads/{id}/read` answers `{ "newly_read": 2 }`.

### Events

Every change appends an event to the project's log. `GET /v1/events?after=<id>&limit=<n>` answers a page, oldest first:

```json
{
  "events": [
    { "id": 3, "at": "2026-03-02T09:00:00Z", "kind": "claimed",
      "payload": { "item": 12, "member": 7, "session": 9, "host": "laptop", "areas": ["default:billing"] },
      "members": { "7": "alice" } }
  ],
  "next": 3
}
```

`after` is the id of the last event the client has, or `0` (the default) for the start of the log; `limit` is 1 to 500
(500 by default). `next` is the cursor for the next page, `null` while the log is empty. `members` maps the ids in the
payload to handles. An event carries ids and handles, never a subject or the text of a message.

| `kind` | `payload` |
|---|---|
| `item_created` | `item`, `external_ref`, `title`, `by` |
| `claimed`, `renewed` | `item`, `member`, `session`, `host`, `areas` |
| `taken_over` | `item`, `member`, `previous`, `session`, `host`, `areas` |
| `released`, `completed` | `item`, `member` |
| `force_released` | `item`, `member`, `by` |
| `reserved` | `item`, `member`, `areas`, `paths` |
| `granted` | `grant`, `item`, `area`, `from`, `to` |
| `grant_revoked` | `grant`, `item`, `area`, `from`, `to`, `cause` (`revoked`, `grantor_left`, `grantee_left`) |
| `thread_started` | `thread`, `author`, `item` |
| `message_posted` | `thread`, `message`, `author` |
| `settings_changed` | `by` |
| `item_updated` | `item`, `by` |
| `dependency_added`, `dependency_removed` | `item`, `blocked_by`, `by` |

`item_updated` is appended when one of an item's reported facts actually changed; read the item for what it says now.

### The event stream

`GET /v1/events/stream` sends the same log as server-sent events (`Content-Type: text/event-stream`,
`Cache-Control: no-cache, no-transform`, `X-Accel-Buffering: no`):

```text
id: 5
data: {"id":5,"at":"…","kind":"message_posted","payload":{…},"members":{…},"message":{…}}

: keep-alive
```

- **Where it starts.** After the event named by the `Last-Event-ID` header (which `EventSource` sends when it
  reconnects), else after `?after=`, else from the newest event on, with nothing replayed. `0` means the start of the
  log.
- **Frames.** Each event's `id` is its id in the log and its `data` is the event as `GET /v1/events` writes it, on one
  line. A `message_posted` event the caller may read also carries `message`. Events about a conversation the caller
  may not read are left out.
- **Keep-alive.** While nothing happens the server sends a comment line (`: keep-alive`) every 25 seconds.
- **Pace.** The server reads the log a page at a time and only when the connection took every frame of the last page,
  so a slow reader holds at most one page.
- **Limits.** A member may hold 8 streams open and the server 128; one more is `503 too_many_streams`.
- **End.** The stream ends when the server shuts down, when the token stops working, or when the store fails. A client
  reconnects with `Last-Event-ID` and misses nothing.

### Settings

`PUT /v1/admin/settings` takes the settings document as JSON (`dibs admin settings apply <file>` reads the same
document as TOML), makes it the project's settings and answers `{ "settings": document, "changed": true }` with the
document in its normal form. Applying the settings in force again answers `"changed": false`. A document that breaks a
rule is `422 invalid_settings`, naming the setting at fault; a body that is no settings document is `400 invalid_json`.

| Setting | Meaning |
|---|---|
| `stale_after` | How long a claim stays live without a sign of life: `24h`; 1 hour to 30 days; default 24 hours. |
| `heartbeat_every` | How often a session sends a heartbeat: `1m`; 1 to 15 minutes; default 1 minute. |
| `edit_policy` | `require-claim` (default): a governed path needs a live claim. `advise`: it does not. |
| `areas` | For a project with one repository: `[{ "name": "billing", "paths": ["packages/billing/**"] }]`. |
| `hot` | For a project with one repository: hot-file rules, first match wins. |
| `repos` | For a project with several: `[{ "id", "remote", "areas", "hot" }]`. The two layouts are never mixed. |

A path glob is matched against a path relative to the repository root: `*` and `?` never cross a `/`, `**` as a whole
segment crosses any number of them, `[…]` and `{a,b}` work as in shells, and matching is case-sensitive. The first area
in declaration order that contains a path governs it. A hot-file rule has `paths`, a `kind` and optionally a `note` (1 to
500 characters):

| `kind` | Effect on a matching path |
|---|---|
| `regenerate` | A generated file anyone may change and nobody merges by hand: always allowed, with the note. |
| `owner` | Only the holder of the `area` it names (an area of the same repository) edits it. |
| `warn-shared` | Anyone may edit; the verdict names the other members' live claims that edited the same file. |
| `free` | Anyone may edit, with nothing said. |

### Admin routes

`POST /v1/admin/members`, body `{ "handle": "bob", "display_name": "Bob Example", "external_login": "bob-gh", "role": "member" }`
(`external_login` and `role` optional; `role` is `admin`, `member` (the default) or `importer`). Answers `201 { "member": member, "token": "dibs_…" }`.
The token is in this answer only. `handle_taken` (`409`) refuses a handle in use.

`POST /v1/admin/members/{handle}/tokens` revokes every token of the member and ends the browser sessions those tokens
opened, then answers `201 { "member": member, "token": "dibs_…" }`.

`DELETE /v1/admin/tokens/{prefix}` revokes the project's token whose prefix is the 8 characters after `dibs_` (with or
without the marker), and answers `{ "token": { "prefix", "member", "created_at", "revoked_at" } }`; revoking a revoked
token changes nothing.

`POST /v1/admin/items/{id}/force-release` ends any member's claim on the item and answers `{ "claim": claim }`
(`no_claim`, `409`, when nobody holds it).

## The web pages

The pages under `/ui` are for people in a browser: `/ui/login` (sign in with a token), `/ui/` (status), `/ui/board`,
`/ui/threads` and `/ui/threads/{id}`, with `/ui/events/stream` feeding the open page. They are rendered on the server and
are not part of the API. The board's *Ready* column is the viewer's ready list, each card with its priority, likely
areas and a link to its tracker, and below it the open items that are not ready with their blockers or the held area;
an item its tracker closed while a claim holds it is marked *Closed upstream*. Their forms post a token of the session they were rendered for, and the session cookie
(`dibs_ui`) is `HttpOnly`, `Secure`, `SameSite=Strict` and scoped to `/ui`. A session ends 12 hours after the page last used it. Every page answer
carries a content security policy that allows only the server's own scripts, styles and connections.
