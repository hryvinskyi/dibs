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
| `403` | `forbidden`, `not_holder` (details: `holder`; for a slot too), `not_grantor`, `not_a_recipient` (an acknowledgement or a delivery for somebody a message did not reach), `not_author` |
| `404` | `not_found` (also: a record of another project, and a conversation the caller may not read), `unknown_slot` (`slot`) |
| `405` | `method_not_allowed` |
| `409` | `held` (details: `holder`, `since`, `last_seen`), `stale` (`holder`, `last_seen`), `area_held` (`area`, `holder`, `item`, `item_ref`), `not_stale` (`holder`, `last_seen`), `nothing_to_take`, `own_claim`, `no_claim`, `item_closed` (`status`), `grantee_has_no_claim`, `handle_taken`, `project_exists`, `dependency_cycle`, `ack_not_required`, `holds_other_slot` (`held`), `slot_not_held`, `not_waiting`, `own_hold`, `not_stalled` (`holder`, `last_seen`), `run_ended`, `taken_over` (`holder`), `delivered` (`session_id`, `at`), `offer_gone` |
| `413` | `body_too_large` |
| `415` | `unsupported_media_type` |
| `422` | `invalid_request` (details: `field`), `unknown_area` (`area`), `invalid_area`, `ambiguous_area` (`candidates`), `unknown_repo` (`repo`), `unlisted_repository` (`remote`), `self_grant`, `not_a_recipient`, `no_recipients`, `invalid_settings`, `self_dependency`, `batch_too_large` (`max`), `invalid_batch`, `duplicate_ref` (`ref`) |
| `429` | `too_many_attempts` (details: `retry_after`), `importance_limited` (`limit`, `frees_at`) |
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
| `POST /v1/items/{id}/claim` | people | Claim an item, or renew your claim; with `wide`, a wide claim over many areas. |
| `POST /v1/items/{id}/takeover` | people | Take over another member's stale claim. |
| `POST /v1/items/{id}/release` | people | Release your claim. |
| `POST /v1/items/{id}/done` | people | Complete your claim. |
| `GET /v1/status` | people | Live claims, stale claims, the areas held, the live wide claims, the caller's ready list and the slots. |
| `POST /v1/edit-check` | people | May I edit these paths? |
| `POST /v1/grants` | people | Let another member edit in an area you hold. |
| `DELETE /v1/grants/{id}` | people | Withdraw a grant you gave. |
| `POST /v1/threads` | people | Start a conversation. |
| `GET /v1/threads` | people | List the conversations you may read. |
| `GET /v1/threads/{id}` | people | A conversation with its messages. |
| `POST /v1/threads/{id}/messages` | people | Post a message. |
| `POST /v1/threads/{id}/read` | people | Mark a conversation read. |
| `GET /v1/inbox` | people | Conversations waiting for you, the messages awaiting your acknowledgement or still to be delivered to you, and the slots offered to you. |
| `POST /v1/messages/{id}/ack` | people | Acknowledge a message you were sent. |
| `POST /v1/messages/{id}/deliver` | people | Claim the delivery of a message to one of your sessions; the first claim wins. |
| `GET /v1/messages/{id}/status` | people | Who read and acknowledged a message you wrote. |
| `GET /v1/slots` | any | Every declared slot: its group's hold, queue and offer. |
| `GET /v1/slots/{name}` | any | One slot, fresh. |
| `POST /v1/slots/{name}/take` | people | Take a slot, renew your hold, or join its queue. |
| `POST /v1/slots/{name}/release` | people | Release a slot you hold; a run gives its exit. |
| `POST /v1/slots/{name}/leave` | people | Leave a slot's queue, or give up the offer to you. |
| `POST /v1/slots/{name}/heartbeat` | people | The heartbeat of the run that holds a slot. |
| `POST /v1/slots/{name}/takeover` | people | Take over another member's stalled hold. |
| `POST /v1/slots/{name}/offer/deliver` | people | Claim the delivery of the slot's offer to you to one of your sessions; the first claim wins. |
| `POST /v1/slots/{name}/run-finished` | import | A forge workflow run finished: end the hold that waited for it. |
| `GET /v1/events` | any | A page of the project's event log. |
| `GET /v1/events/stream` | people | The event log as server-sent events. |
| `POST /v1/admin/members` | admin | Add a member. |
| `POST /v1/admin/members/{handle}/tokens` | admin | Rotate a member's token. |
| `DELETE /v1/admin/tokens/{prefix}` | admin | Revoke one token. |
| `PUT /v1/admin/settings` | admin | Replace the project's settings. |
| `POST /v1/admin/items/{id}/force-release` | admin | End any member's claim. |
| `POST /v1/admin/slots/{name}/force-release` | admin | End any member's hold on a slot. |

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
claim    { item, holder, session_id, host, areas: ["repo:area", …], wide: { areas: ["repo:area", …], reason } | null,
           claimed_at, renewed_at, last_seen }
grant    { id, item, area, from, to, created_at }
thread   { id, subject, item | null, author, to_project, to: [handle, …], created_at }
message  { id, thread, author, label, body, importance: "normal" | "high" | "urgent", ack_required, created_at,
           acks?: [{ member, acked_at | null }, …] }
summary  { thread, messages, unread, last_message_at | null, importance, awaiting_ack }
```

`repo` of an item is the project's repository that its reference names, when the project lists it; `unlisted_remote` is
the repository the reference names when the project does not list it. `priority` (0 is soonest, 4 latest, 2 when nothing
said otherwise), `areas_hint` (the areas the work will probably touch; advisory, no claim declares them),
`source_status` (the outside tracker's own state, as it shows it), `closed` (the tracker closed the item; a claim on it
stays) and `source` (the importer's source that lists it) are what an importer reports. An *item ref* is how an answer
mentions another item: `link` is the tracker's page for its reference (a `gh` reference links to its GitHub issue), or
`null`.

A message's `importance` says how much it asks for its recipients' attention, and `ack_required` whether it asks each of
them to acknowledge it; `acks` is in a thread read only, for a message that asks. A summary's `importance` is the highest
among its messages and `awaiting_ack` how many of them ask the caller for an acknowledgement not given yet.

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
  "blocked_by": [], "item": { "id": 12, "ref": "gh:acme/widgets#7", "title": "…", "link": "…" } }
```

`outcome` is `new` for a first claim and `renewed` when the caller already held the item (a stale claim of the caller's
included); a renewal adds the requested areas to those held and lists in `dropped_areas` those it no longer holds
because another member holds them now, another live wide claim names them now, or the settings dropped them. Refusals:
`held` (another member's live claim holds the item), `stale` (their claim is stale: only a takeover may take it),
`area_held` (another member's live claim holds an area), `unknown_area`, `invalid_area`, `ambiguous_area`,
`item_closed`. An area belongs to one member at a time. A claim on a blocked item is made all the same — dependent work
is often started in parallel — and `blocked_by` lists, as item refs, the items it waits for that are neither done nor
closed, for the client to warn about. `item` is the claimed item as an item ref, so a client names it by its reference.

**Wide claims.** A change that cuts across many areas at once (a dependency upgrade, a rename across packages) is claimed
wide: `{ "session_id": 7, "wide": { "areas": ["billing", "search"], "reason": "Upgrade the HTTP client" } }`, the
reason 1 to 300 characters. A wide claim **holds none of the areas it names**, so it never refuses anyone's claim of
them, and nobody's claim refuses it; it may still hold areas of its own under `areas` (an edit outside its scope adds the
area there, as for any claim), but never one it also names. One area is named by one live wide claim at a time. In the
same transaction every member whose live claim holds one of the named areas is sent an **announcement**: a conversation
about the wide claim's item addressed to them alone, written in the wide claim's member's name under the label `dibs`,
whose one message is `high` and asks to be acknowledged. Its text says dibs sent it for that member, why, and which of
the recipient's areas it is about. `announced` in the answer lists them:

```json
{ "outcome": "new", "claim": { "item": 50, "holder": "bob", "areas": [],
    "wide": { "areas": ["default:billing", "default:search"], "reason": "Upgrade the HTTP client" }, "…": "…" },
  "announced": [{ "to": "alice", "areas": ["default:billing"], "message_id": 91, "thread_id": 90 }],
  "inside_wide": [], "…": "…" }
```

**A holder's acknowledgement of the announcement is their grant**: for each area it asked about that they still hold
through a live claim, the wide claim's member may edit there for the wide claim's item. The grant is like any other: in
force while both claims are live, ended (`grant_revoked`) when either ends, revocable by the holder. Until a holder
acknowledges, the wide claim's member's edits in their area answer `held_by` with `reason: "awaiting_ack"`; in a named
area nobody holds they answer `allowed`, and nothing is added to the claim's `areas`. A member who comes to hold a named
area later — by claiming it, by a takeover, or by an edit that adds it — is sent the same announcement in the same
transaction, and the claim answer lists the area in `inside_wide`:
`[{ "area": "default:billing", "holder": "bob", "item": item ref, "message_id": 93 }]` (`message_id` is the
announcement waiting for them, or `null`). Nobody is asked twice about one area while an announcement to them waits.
A renewal adds the requested areas to the scope and takes the new reason; without `wide` the claim keeps its scope. A
takeover carries the scope on, minus areas another live wide claim names now, and announces it again in the new
member's name. Refusals: `wide_overlap` (`409`: another live wide claim names an area; `details` name its member,
item and the area), `wide_and_held` (`422`: the claim would both hold and name an area), `invalid_wide` (`422`: no area
named), `invalid_request` for a missing or invalid `wide.reason` or a missing `wide.areas`, and the area refusals above.

`POST /v1/items/{id}/takeover`, body `{ "session_id": 7, "reason": "…", "areas": [] }`, takes over another member's
stale claim and tells them the reason in a conversation about the item, as a `high` message that asks for no
acknowledgement. Answers
`{ "claim": …, "previous_holder": "bob", "dropped_areas": [], "thread_id": 31, "item": item ref, "announced": [] }`. Refusals: `not_stale` (the claim is
live), `own_claim`, `nothing_to_take`, `area_held`, `wide_overlap`.

`POST /v1/items/{id}/release` and `POST /v1/items/{id}/done` take no body and answer
`{ "claim": claim, "item": item ref }`, the claim that ended and its item; the item is open again, or done. Refusals: `not_holder` (`403`), `no_claim` (`409`).

`GET /v1/status` answers

```json
{
  "live":  [{ "item": item, "claim": claim }],
  "stale": [{ "item": item, "claim": claim }],
  "holds": [{ "area": "default:billing", "holder": "alice", "item": 12 }],
  "wide":  [{ "item": item, "claim": claim,
              "areas": [{ "area": "default:billing", "state": "waiting", "holder": "alice", "item": 12 },
                        { "area": "default:search", "state": "free", "holder": null, "item": null }] }],
  "ready": { "ready": [], "excluded": [] }
}
```

`holds` never lists an area a wide claim names. `wide` lists the live wide claims by item, each area with where it
stands for the wide claim's member: `free` (nobody else holds it), `granted` (its holder acknowledged the announcement,
or granted the edits) or `waiting` (its holder has not). `ready` is the caller's ready list, as `GET /v1/ready` answers
it.

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
| `allowed_with_note` | The caller may edit; a hot-file rule has something to say. | `rule`, `note`, `touched_by: [{ holder, item, item_ref }]` |
| `needs_claim` | The path is governed, the policy requires a claim, and the caller holds none that covers it. | |
| `held_by` | Another member's live claim holds the path's area. | `holder`, `item`, `item_ref` (its reference, or `null`), `area`, `reason`: `ask_holder` (message them or ask for a grant), or `awaiting_ack` (the caller's live wide claim names the area and the holder has not acknowledged its announcement yet) |
| `granted` | The holder granted the caller edits in the area, by a grant or by acknowledging the caller's wide claim's announcement. | `by`, `area` |

A path that no area or rule governs is `allowed`. Refusals: `unknown_repo` (`422`), `invalid_request` for a missing
field or a path that is not a relative path, `not_found` for a session that is not the caller's.

### Grants

`POST /v1/grants`, body `{ "item": 14, "area": "default:billing", "to": "bob" }`: the caller, who holds the area, lets
`to` edit in it, for `to`'s live claim on `item`. Answers `201 { "grant": grant, "created": true, "item": item ref }`,
or `200` with `"created": false` when the same grant exists. Refusals: `not_holder` (`403`), `self_grant`, `grantee_has_no_claim`,
`unknown_area`, `not_found`. A grant ends when it is revoked, when its grantor no longer holds the area, or when the
grantee's claim ends.

`DELETE /v1/grants/{id}` answers `{ "grant": grant }`; only the grantor may (`not_grantor`, `403`).

### Conversations

`POST /v1/threads`, body

```json
{ "subject": "Billing rounding", "to": ["alice"], "item": 12, "label": "laptop/cli", "body": "May I touch round.rs?",
  "importance": "high", "ack_required": true }
```

`to` names members by handle; with `"to_project": true` instead the conversation goes to every person in the project;
naming both, or neither, is `invalid_request`. `item` (optional) is the work item it is about, `label` the session
label it is signed with. `importance` is `normal` (the default), `high` or `urgent`; `ack_required` (default `false`)
asks each recipient to acknowledge the message. Answers `201 { "thread": thread, "message": message }`. Refusals:
`not_found` for an unknown recipient or item, `not_a_recipient` for an integration, `no_recipients` for an empty `to`,
`invalid_request` for an importance that is none of the three, and `importance_limited` (`429`) for a member's `high`
or `urgent` message past the project's `raised_per_hour` (ten unless the settings say otherwise) within the last hour:
its sentence and `frees_at` say when the next one is allowed, and the same message at `normal` goes through. Only what a
member writes through these routes counts; what dibs sends itself (a wide claim's announcements, a slot's notices)
never does.

`GET /v1/threads?item=12` answers `{ "threads": [summary, …] }`, the conversations the caller may read, newest activity
first. `GET /v1/inbox` answers the same shape for the conversations waiting for the caller; with `unread=true` (or `1`)
only those with messages they have not read. With `unacked=1` (or `true`) the answer also carries
`"unacked": [{ "thread": thread, "message": message }, …]`, newest first: every message that asks the caller for an
acknowledgement they have not given. A conversation the caller may not read is reported as missing, the same as one that
does not exist. Every inbox answer carries `"offers": [{ "slot": "deploy:prod", "until": "…" }, …]`: the slots offered
to the caller now, each theirs if they take it before `until` (what time changed about the slots is applied first).
With `deliverable=1` the answer also carries `"deliverable": [{ "thread": thread, "message": message, "awaiting_ack":
true }, …]`, newest first, at most 20: the messages still to be delivered to the caller's agent while it works — written
by someone else within the last day, reaching the caller, `high` or `urgent` and unread or awaiting the caller's
acknowledgement, and not yet delivered to any of the caller's sessions. A wide claim's announcement whose wide claim has
ended is in neither `unacked` nor `deliverable`, and is not counted as awaiting an acknowledgement: it asks for nothing
any more.

`GET /v1/threads/{id}` answers `{ "thread": thread, "messages": [message, …] }`, oldest first; a message that asks to be
acknowledged carries `acks`, each recipient ordered by handle with when they acknowledged it (`null` while they have
not). Reading does not mark it read. `POST /v1/threads/{id}/messages`, body
`{ "label": "laptop/cli", "body": "…", "importance": "normal", "ack_required": false }` (the last two optional, as when
starting a thread, and held to the same hourly limit), answers `201 { "message": message }`.
`POST /v1/threads/{id}/read` answers `{ "newly_read": 2 }`.

### Delivery to a running session

A pressing message or a slot offered to a member reaches their agent between its tool calls (the guard's hooks show it).
A member runs several sessions, so each subject is delivered once per member: the first of their sessions to claim it
delivers it, and every later claim, from any session, is refused. Claiming appends no event.

`POST /v1/messages/{id}/deliver`, body `{ "session_id": 9 }`, claims a message for the caller's session `session_id`.
Answers `201 { "delivery": { "subject": "41", "session_id": 9, "at": "…" } }`. Refusals: `delivered` (`409`, details
the `session_id` that claimed it and `at`), `not_a_recipient` (`403`) for a message the caller wrote or was not sent,
`not_found` for a message in a conversation the caller may not read or a session that is not theirs.

`POST /v1/slots/{name}/offer/deliver`, body `{ "session_id": 9, "until": "2026-03-02T09:05:00Z" }`, claims the offer of
the slot to the caller that lapses at `until`, as the inbox gave it; its `subject` is `offer:<slot>:<until in unix
nanoseconds>`, so a new offer after one lapsed is claimed anew. Refusals: `delivered` (`409`), `offer_gone` (`409`) when
the slot is not on offer to the caller until that time (taken, lapsed, or never theirs; what time changed is applied
first), `invalid_request` for a missing `session_id` or an `until` that is no RFC 3339 time.

### Acknowledgements

A message reaches everyone who may read its thread but its author: the thread's author and the members it names, or, for
a thread addressed to the project, every person who was in the project when the message was written. An integration is
never a recipient. A message never changes; an acknowledgement is a record of its own, one per recipient.

`POST /v1/messages/{id}/ack` takes no body and records the caller's acknowledgement. Answers
`201 { "ack": { "message", "thread", "member", "at" }, "created": true, "granted": [] }` and appends `acked`.
Acknowledging a wide claim's announcement also grants its member edits, for the wide claim's item, in each area it asked
about that the caller still holds through a live claim: `granted` lists those grants, and each appends
`wide_granted_by_ack`. Acknowledging again is harmless, so a client may retry after a lost answer: it answers `200` with
the first acknowledgement (its original `at`), `"created": false` and `"granted": []`, and records and appends nothing.
An announcement whose wide claim has ended (released, completed or taken over) grants nothing when acknowledged; the
answer then carries `"announcement_ended": true`. Refusals: `not_a_recipient` (`403`: the message did not
reach the caller, or the caller wrote it), `ack_not_required` (`409`: the message asks for none), `not_found` for a
message in a conversation the caller may not read.

`GET /v1/messages/{id}/status` shows the author of a message where each recipient stands:

```json
{ "thread": thread, "message": message,
  "recipients": [{ "member": "bob", "read_at": "2026-03-02T09:02:00Z", "acked_at": "2026-03-02T09:03:00Z" },
                 { "member": "carol", "read_at": null, "acked_at": null }] }
```

Recipients are ordered by handle. Refusals: `not_author` (`403`) for a message somebody else wrote, `not_found` for one
in a conversation the caller may not read.

### Slots

A slot is a named shared action one member does at a time — a deploy, a release, a migration of a shared database. It is
declared in the settings (below); the routes take it, wait for it and end it. A hold is the member's, like a claim: any
session of theirs acts on it. Slots of one `group` exclude each other: one hold across the group at a time, one queue
and one offer. Every slot route applies what time changed before it answers — a hold past its expiry ends, a run's hold
without a heartbeat for 2 minutes is stalled, an offer past its deadline lapses and its member leaves the queue, a waiter
silent for longer than `max_wait` loses their place — and the server does the same every 30 seconds for every project
with a hold, a waiter or an offer, so a freed slot is offered to the first waiter even when nobody touches it. The slot
name in a path is percent-encoded like any segment; a name that is no slot name is answered `404 unknown_slot`.

```text
slot   { name, group | null, max_hold: "30m", offer_for: "5m", max_wait: "2h", run_only, freezes: ["api:refs/heads/main"],
         note | null, hold: hold | null, queue: [waiter, …], offer: offer | null }
hold   { slot, holder, purpose, item | null, session_id, host,
         liveness: { "mode": "session" } | { "mode": "run", "run": 7 } | { "mode": "until_run", "workflow", "sha" },
         taken_at, expires_at, last_seen, stalled }
waiter { member, slot, purpose, since, last_seen }
offer  { member, slot, purpose, until }
```

A slot shows its group's hold, queue and offer; `hold.slot` names the slot held when it is another of the group.

`GET /v1/slots` answers `{ "slots": [slot, …] }` in declaration order, `GET /v1/slots/{name}` answers `{ "slot": slot }`.
An integration may read them.

`POST /v1/slots/{name}/take`, body `{ "session_id": 9, "purpose": "ship 2.4", "item": 12, "mode": "session" }` (`item`
and `mode` optional). `purpose` is 1 to 200 characters every member reads — never a credential, a host or a customer's
data. `mode` is `session` (the default: the hold ends at its expiry), `run` (kept by the heartbeats of
`POST …/heartbeat`; the answer names the run), or `until_run` with `workflow` and `sha` (kept until the worker reports
that run finished, or its expiry). Answers
`{ "outcome": "taken" | "renewed" | "queued", "position": 1 | null, "run_id": 7 | null, "slot": slot }`:

- a free slot, or one on offer to the caller, is `taken`: it expires `max_hold` from now (`slot_taken`);
- the caller's own hold is `renewed` to `max_hold` from now, but while somebody waits never past twice `max_hold` from
  when it was taken, and never shortened (`slot_renewed`); a plain take keeps a hold kept by a run or a workflow;
- otherwise the caller is `queued` behind the group's hold or its offer, at `position` from 1 (`slot_queued`). Taking
  again while queued keeps the place, refreshes it and records the slot and purpose asked for now.

Refusals: `unknown_slot` (`404`), `holds_other_slot` (`409`, details `held`) when the caller holds another slot of the
group, `not_found` for a session or item not the caller's, `invalid_request` for a missing purpose or a mode without
its fields.

`POST /v1/slots/{name}/release`, body `{}` or, from a run, `{ "run_id": 7, "exit": { "code": 3 } }` (`exit` is
`{"code": n}` or `{"signal": "SIGTERM"}`). Answers `{ "released": hold, "slot": slot }`: the group's first waiter is
offered the slot for `offer_for` (`slot_released` with `exit`, then `slot_offered`). Refusals: `slot_not_held`
(`409`), `not_holder` (`403`, naming the holder), `run_ended` (`409`) when `run_id` no longer holds it.

`POST /v1/slots/{name}/leave` takes no body: the caller leaves the group's queue, or gives up the offer to them, which
passes to the next waiter (`slot_left`). Answers `{ "slot": slot }`; `not_waiting` (`409`) otherwise.

`POST /v1/slots/{name}/heartbeat`, body `{ "run_id": 7 }`: the run's hold is seen now; a stalled one is live again
(`slot_resumed`). Answers `{ "slot": slot }`. When the hold is no longer the run's: `taken_over` (`409`, naming who
holds it now) or `run_ended` (`409`). A stalled hold offers nothing and does not end at its expiry.

`POST /v1/slots/{name}/takeover`, body `{ "session_id": 9, "purpose": "finish the deploy", "reason": "the deploy log
stopped; nothing runs" }`: takes over another member's **stalled** hold and sends them `reason` in a conversation of its
own (`slot_stall_taken_over`). Answers `{ "previous": hold, "thread_id": 31, "slot": slot }`. Refusals:
`slot_not_held`, `own_hold`, `not_stalled` (`409`, naming the holder and `last_seen`).

`POST /v1/slots/{name}/run-finished`, body `{ "workflow": "deploy.yml", "sha": "abc1234", "conclusion": "success" }`,
for an integration watching the forge (the importer or an admin): ends the hold taken `until_run` for exactly that
workflow and commit, with `{"conclusion": …}` as its exit, and answers `{ "matched": true }`. Any report that matches no
hold — a repeat, a slot released since — changes nothing and answers `{ "matched": false }`, so it may be sent again.

`POST /v1/admin/slots/{name}/force-release`, body `{ "session_id": 9, "reason": "…" }`, ends any member's hold, sends
the holder `reason`, and offers the slot on (`slot_force_released`, cause `admin`). Answers
`{ "released": hold, "holder": "bob", "thread_id": 31 | null, "slot": slot }`; `slot_not_held` (`409`) when nobody
holds it.

A settings change that no longer declares a slot ends its hold, its queue and its offer in the same change
(`slot_force_released`, cause `slot_removed`). The slot events carry the slot, the members and, for `slot_taken`,
`slot_queued` and `slot_stall_taken_over`, the purpose; they never carry a reason.

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
| `acked` | `thread`, `message`, `by` |
| `wide_claimed` | `item`, `member`, `areas` (every area the wide scope names now) |
| `wide_granted_by_ack` | `grant`, `item`, `area`, `by` (the holder who acknowledged), `to` (the wide claim's member) |

`item_updated` is appended when one of an item's reported facts actually changed; read the item for what it says now.
`wide_claimed` is appended when a claim becomes wide or its scope grows; a renewal that names nothing new appends none.
A grant an acknowledgement made ends like any grant, with `grant_revoked` naming its id.
`acked` is about a conversation, like `thread_started` and `message_posted`: a caller who may not read the conversation
does not see it.

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
| `raised_per_hour` | How many `high` or `urgent` messages one member may write within an hour: 1 to 1000; default 10. What dibs writes itself is not counted. |
| `areas` | For a project with one repository: `[{ "name": "billing", "paths": ["packages/billing/**"] }]`. |
| `hot` | For a project with one repository: hot-file rules, first match wins. |
| `repos` | For a project with several: `[{ "id", "remote", "areas", "hot" }]`. The two layouts are never mixed. |
| `slots` | The shared actions one member does at a time; see below. |

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

A slot has a `name` (1 to 64 characters: lowercase letters, digits and `:._-`) and:

| Field | Meaning |
|---|---|
| `max_hold` | Required: the longest one hold lasts before it is renewed, 1 minute to 24 hours. |
| `offer_for` | How long an offer to the first waiter stands: 1 minute to 1 hour; default 5 minutes. |
| `max_wait` | How long a waiter keeps their place without a sign of life: 1 minute to 24 hours; default 2 hours. |
| `commands` | Up to 20 `{ "matches": "<regex>", "unless": "<regex>" }`, each 1 to 500 characters, matched against one simple command of a command line (below). |
| `run_only` | Its commands run only inside `dibs slot run`: a hold from a plain take does not let them through. |
| `group` | Slots of one group exclude each other. |
| `freezes` | Branch refs nobody but the holder may push while it is held: `api:refs/heads/main`, or `refs/heads/main` in a project with one repository. |
| `note` | Shown with the slot, 1 to 500 characters. |

The patterns are regular expressions of the Rust `regex` crate, which match in time linear in the input and have no
look-around (`unless` is the exclusion). A command line is split into simple commands before a pattern sees it — on
`;`, `&&`, `||`, `|`, `&`, parentheses and line breaks outside quotes, without `VAR=value`, `env`, `sudo`, `nohup`,
`time`, `command`, `exec` and a leading `./`, and with the script of `sh -c` split too — so `^deploy\.sh prod` matches
`cd ops && ./deploy.sh prod` and not `echo "./deploy.sh prod"`. `dibs slot test <settings.toml> -- <command line>`
shows what a line needs without a server.

The command line itself never leaves the member's machine: the guard splits and judges it there, against the settings
it keeps, and asks the server only `GET /v1/slots/{name}` for the few slots its verdict reads — the one whose pattern
matches, and those that freeze a ref a `git push` in the line names. A line no slot governs asks the server nothing.
`dibs check --push`, run from git's `pre-push` hook, judges the refs git says a push goes to by the same rule.

### Admin routes

`POST /v1/admin/members`, body `{ "handle": "bob", "display_name": "Bob Example", "external_login": "bob-gh", "role": "member" }`
(`external_login` and `role` optional; `role` is `admin`, `member` (the default) or `importer`). Answers `201 { "member": member, "token": "dibs_…" }`.
The token is in this answer only. `handle_taken` (`409`) refuses a handle in use.

`POST /v1/admin/members/{handle}/tokens` revokes every token of the member and ends the browser sessions those tokens
opened, then answers `201 { "member": member, "token": "dibs_…" }`.

`DELETE /v1/admin/tokens/{prefix}` revokes the project's token whose prefix is the 8 characters after `dibs_` (with or
without the marker), and answers `{ "token": { "prefix", "member", "created_at", "revoked_at" } }`; revoking a revoked
token changes nothing.

`POST /v1/admin/items/{id}/force-release` ends any member's claim on the item and answers
`{ "claim": claim, "item": item ref }` (`no_claim`, `409`, when nobody holds it).

## The web pages

The pages under `/ui` are for people in a browser: `/ui/login` (sign in with a token), `/ui/` (status, with the live wide
claims and, for each of their areas, whether it is free, acknowledged by its holder or waiting for them), `/ui/board`,
`/ui/threads` and `/ui/threads/{id}`, with `/ui/events/stream` feeding the open page. They are rendered on the server and
are not part of the API. The board's *Ready* column is the viewer's ready list, each card with its priority, likely
areas and a link to its tracker, and below it the open items that are not ready with their blockers or the held area;
an item its tracker closed while a claim holds it is marked *Closed upstream*. Above the columns, the live wide claims are
listed with their reason and, for each area, who has acknowledged them; the area filter keeps the wide claims that name
the area, the member filter those of the member and those that wait for the member's acknowledgement. When a claim or an
item changes, the page fetches `GET /ui/board/fragment` with the same query string, once per burst of events and not
while the tab is hidden, and swaps its columns in, except while a form in them is in use, when it shows its reload
banner instead. The fragment is the columns' HTML alone, with the forms' session tokens, marked by the header
`X-Dibs-Fragment: board` and `Cache-Control: no-store`; without a live session it answers `401` with the error shape,
never the sign-in page; the columns carry the time they were rendered at, which the page's *updated* time shows after a
swap. A take-over made from the board returns to the board with the same filter; the form's return path is followed
only when it is a path of this service, and anything else returns to `/ui/board`. The threads list marks each conversation's
highest importance and whether it holds a message awaiting the viewer's acknowledgement; a thread page marks a raised
importance on the messages that carry it and, under a message that asks to be acknowledged, lists its recipients with who
has acknowledged, updated live from the `acked` events, and offers a recipient who has not an *Acknowledge* button
(`POST /ui/threads/{id}/messages/{message}/ack`).
A reply from the page may ask everyone to acknowledge it. Their forms post a token of the session they were rendered for, and the session cookie
(`dibs_ui`) is `HttpOnly`, `Secure`, `SameSite=Strict` and scoped to `/ui`. A session ends 12 hours after the page last used it. Every page answer
carries a content security policy that allows only the server's own scripts, styles and connections.
