# HTTP API

The server answers JSON under `/v1`, one health probe, and the pages of the web UI under `/ui`. The command line, the
MCP server and the hooks are clients of this API; the examples below use `curl` and a made-up project `widgets` with the
members `alice` and `bob`.

## Conventions

- **Format.** Requests with a body are JSON and carry `Content-Type: application/json`. Answers are JSON. Times are
  RFC 3339 in UTC (`2026-03-02T09:00:00Z`). Ids are positive whole numbers. Members are named by their handle. An area is
  written `repo:area` (`api:billing`); in a project with one repository the repository is called `default`. An external
  reference is `scheme:value` (`gh:acme/widgets#42`); a reference never appears in a path, only in a query string or a
  body, because it holds `/` and `#`.
- **Caching.** Every answer carries `Cache-Control: no-store` unless its route sets its own.
- **Limits.** A request body is at most 128 KiB. A request is answered within 10 seconds or refused with `timeout`
  (the event stream is bounded only until its answer starts). A message body is at most 16 KiB.
- **Unknown fields.** A field a route does not read is ignored in a request, except in the settings document, which
  refuses unknown fields.

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
integration holds no claim, receives no message and reads no conversation: the routes marked *people* answer it
`403 forbidden`. Routes marked *admin* answer `403 forbidden` to every other role, before they read the request.

## Errors

Every failure has the same shape:

```json
{
  "error": {
    "code": "area_held",
    "message": "The area api:billing is held by another member.",
    "details": { "area": "api:billing", "holder": "alice", "item": 12 }
  }
}
```

`code` is stable and written for programs; `message` is an English sentence for the person reading it and never carries
an internal error's text; `details` is present only when the refusal names something the caller can act on. The server
logs the cause of an internal failure and answers `internal`.

| Status | Codes |
|---|---|
| `400` | `invalid_json` (the body is not JSON of the shape the route expects), `invalid_query` |
| `401` | `unauthorized` |
| `403` | `forbidden`, `not_holder` (details: `holder`), `not_grantor` |
| `404` | `not_found` (also: a record of another project, and a conversation the caller may not read) |
| `405` | `method_not_allowed` |
| `409` | `held` (details: `holder`, `since`, `last_seen`), `stale` (`holder`, `last_seen`), `area_held` (`area`, `holder`, `item`), `not_stale` (`holder`, `last_seen`), `nothing_to_take`, `own_claim`, `no_claim`, `item_closed` (`status`), `grantee_has_no_claim`, `handle_taken`, `project_exists` |
| `413` | `body_too_large` |
| `415` | `unsupported_media_type` |
| `422` | `invalid_request` (details: `field`), `unknown_area` (`area`), `invalid_area`, `ambiguous_area` (`candidates`), `unknown_repo` (`repo`), `unlisted_repository` (`remote`), `self_grant`, `not_a_recipient`, `no_recipients`, `invalid_settings` |
| `429` | `too_many_attempts` (details: `retry_after`) |
| `500` | `internal` |
| `503` | `timeout`, `too_many_streams` |

## Routes

*Auth* is `any` (every role), `people` (not an integration) or `admin`.

| Method and path | Auth | What it does |
|---|---|---|
| `GET /health` | none | The process answers: `{"ok":true}`. |
| `GET /v1/me` | any | The caller, their project and its settings. |
| `GET /v1/members` | any | The project's members. |
| `POST /v1/sessions` | people | Register a session, or update the one of the same label. |
| `POST /v1/heartbeat` | people | A session's sign of life. |
| `GET /v1/items` | any | List work items. |
| `POST /v1/items` | any | Create a work item, or return the one a reference names. |
| `GET /v1/items/{id}` | any | One item and its claim. |
| `POST /v1/items/{id}/claim` | people | Claim an item, or renew your claim. |
| `POST /v1/items/{id}/takeover` | people | Take over another member's stale claim. |
| `POST /v1/items/{id}/release` | people | Release your claim. |
| `POST /v1/items/{id}/done` | people | Complete your claim. |
| `GET /v1/status` | people | Live claims, stale claims and the areas held. |
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
           repo | null, unlisted_remote | null, created_by, created_at }
claim    { item, holder, session_id, host, areas: ["repo:area", …], claimed_at, renewed_at, last_seen }
grant    { id, item, area, from, to, created_at }
thread   { id, subject, item | null, author, to_project, to: [handle, …], created_at }
message  { id, thread, author, label, body, created_at }
summary  { thread, messages, unread, last_message_at | null }
```

`repo` of an item is the project's repository that its reference names, when the project lists it; `unlisted_remote` is
the repository the reference names when the project does not list it.

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

### Claims

`POST /v1/items/{id}/claim`, body `{ "session_id": 7, "areas": ["billing"] }`. An area is written as a person writes
it: `repo:area`, or a bare name that exactly one repository declares. Answers

```json
{ "outcome": "new", "claim": { "item": 12, "holder": "alice", "areas": ["default:billing"], "…": "…" }, "dropped_areas": [] }
```

`outcome` is `new` for a first claim and `renewed` when the caller already held the item (a stale claim of the caller's
included); a renewal adds the requested areas to those held and lists in `dropped_areas` those it no longer holds
because another member holds them now or the settings dropped them. Refusals: `held` (another member's live claim holds
the item), `stale` (their claim is stale: only a takeover may take it), `area_held` (another member's live claim holds
an area), `unknown_area`, `invalid_area`, `ambiguous_area`, `item_closed`. An area belongs to one member at a time.

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
  "holds": [{ "area": "default:billing", "holder": "alice", "item": 12 }]
}
```

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
    "holder": "bob", "item": 14, "area": "default:billing", "reason": "ask_holder" }
] }
```

| `verdict` | Meaning | Extra fields |
|---|---|---|
| `allowed` | The caller may edit. | |
| `allowed_with_note` | The caller may edit; a hot-file rule has something to say. | `rule`, `note`, `touched_by: [{ holder, item }]` |
| `needs_claim` | The path is governed, the policy requires a claim, and the caller holds none that covers it. | |
| `held_by` | Another member's live claim holds the path's area. | `holder`, `item`, `area`, `reason` (`ask_holder`: message them or ask for a grant) |
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
(`external_login` and `role` optional; `role` is `member` by default). Answers `201 { "member": member, "token": "dibs_…" }`.
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
are not part of the API: their forms post a token of the session they were rendered for, and the session cookie
(`dibs_ui`) is `HttpOnly`, `Secure`, `SameSite=Strict` and scoped to `/ui`. A session ends 12 hours after the page last used it. Every page answer
carries a content security policy that allows only the server's own scripts, styles and connections.
