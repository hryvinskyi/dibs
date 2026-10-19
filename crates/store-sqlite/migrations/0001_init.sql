-- Every record of every project.
--
-- Each table carries `project_id`, and every key a record is found by starts with it, so a query
-- that forgets the project finds nothing rather than another project's rows. A reference to another
-- record carries the project too, so a row can only point at a record of its own project. Foreign
-- keys are checked when the transaction commits, so a transaction may write rows in any order.
--
-- Times are nanoseconds since the Unix epoch, in UTC. Ids are the numbers the id generator handed
-- out, except event ids, which the log assigns. Paths and areas are qualified by their repository
-- (`repo` beside `path` or `area`; `repo:name` inside JSON).

CREATE TABLE projects (
    id INTEGER PRIMARY KEY NOT NULL,
    slug TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL
) STRICT;

CREATE TABLE members (
    project_id INTEGER NOT NULL REFERENCES projects (id) DEFERRABLE INITIALLY DEFERRED,
    id INTEGER NOT NULL,
    handle TEXT NOT NULL,
    display_name TEXT NOT NULL,
    external_login TEXT,
    role TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (project_id, id),
    UNIQUE (project_id, handle)
) STRICT;

-- An API token, kept as its hash. The prefix names it to people without revealing it.
CREATE TABLE tokens (
    hash TEXT PRIMARY KEY NOT NULL,
    project_id INTEGER NOT NULL REFERENCES projects (id) DEFERRABLE INITIALLY DEFERRED,
    prefix TEXT NOT NULL,
    member_id INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    revoked_at INTEGER,
    UNIQUE (project_id, prefix),
    UNIQUE (project_id, hash),
    FOREIGN KEY (project_id, member_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

CREATE INDEX tokens_by_member ON tokens (project_id, member_id, created_at);

CREATE TABLE sessions (
    project_id INTEGER NOT NULL,
    id INTEGER NOT NULL,
    member_id INTEGER NOT NULL,
    host TEXT NOT NULL,
    label TEXT NOT NULL,
    agent TEXT NOT NULL,
    push INTEGER NOT NULL CHECK (push IN (0, 1)),
    last_seen INTEGER NOT NULL,
    PRIMARY KEY (project_id, id),
    UNIQUE (project_id, member_id, label),
    FOREIGN KEY (project_id, member_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

-- A work item. Its repository is a listed repository's id (`repo`), the remote of one the project
-- does not list (`remote`), or neither.
CREATE TABLE items (
    project_id INTEGER NOT NULL,
    id INTEGER NOT NULL,
    external_ref TEXT,
    title TEXT NOT NULL,
    status TEXT NOT NULL,
    repo TEXT,
    remote TEXT,
    created_by INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (project_id, id),
    UNIQUE (project_id, external_ref),
    CHECK (repo IS NULL OR remote IS NULL),
    FOREIGN KEY (project_id, created_by) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

-- At most one claim per item, live or stale. `areas` is a JSON array of qualified area names.
CREATE TABLE claims (
    project_id INTEGER NOT NULL,
    item_id INTEGER NOT NULL,
    member_id INTEGER NOT NULL,
    session_id INTEGER NOT NULL,
    host TEXT NOT NULL,
    areas TEXT NOT NULL CHECK (json_valid(areas) AND json_type(areas) = 'array'),
    claimed_at INTEGER NOT NULL,
    renewed_at INTEGER NOT NULL,
    last_seen INTEGER NOT NULL,
    PRIMARY KEY (project_id, item_id),
    FOREIGN KEY (project_id, item_id) REFERENCES items (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, member_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, session_id) REFERENCES sessions (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

CREATE INDEX claims_by_member ON claims (project_id, member_id);
CREATE INDEX claims_by_last_seen ON claims (project_id, last_seen);

-- A path recorded as shared under the claim on an item. It refers to the item rather than the
-- claim: it is removed explicitly when the claim ends, never as a side effect of replacing it.
CREATE TABLE shared_paths (
    project_id INTEGER NOT NULL,
    item_id INTEGER NOT NULL,
    repo TEXT NOT NULL,
    path TEXT NOT NULL,
    PRIMARY KEY (project_id, item_id, repo, path),
    FOREIGN KEY (project_id, item_id) REFERENCES items (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

CREATE TABLE grants (
    project_id INTEGER NOT NULL,
    id INTEGER NOT NULL,
    item_id INTEGER NOT NULL,
    repo TEXT NOT NULL,
    area TEXT NOT NULL,
    from_member INTEGER NOT NULL,
    to_member INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (project_id, id),
    FOREIGN KEY (project_id, item_id) REFERENCES items (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, from_member) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, to_member) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

-- A conversation, addressed to the whole project (`to_project`) or to the members listed in
-- `thread_recipients`.
CREATE TABLE threads (
    project_id INTEGER NOT NULL,
    id INTEGER NOT NULL,
    subject TEXT NOT NULL,
    item_id INTEGER,
    author_id INTEGER NOT NULL,
    to_project INTEGER NOT NULL CHECK (to_project IN (0, 1)),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (project_id, id),
    FOREIGN KEY (project_id, item_id) REFERENCES items (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, author_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

CREATE INDEX threads_by_item ON threads (project_id, item_id);

CREATE TABLE thread_recipients (
    project_id INTEGER NOT NULL,
    thread_id INTEGER NOT NULL,
    member_id INTEGER NOT NULL,
    PRIMARY KEY (project_id, thread_id, member_id),
    FOREIGN KEY (project_id, thread_id) REFERENCES threads (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, member_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

-- A message. Nothing updates or deletes one.
CREATE TABLE messages (
    project_id INTEGER NOT NULL,
    id INTEGER NOT NULL,
    thread_id INTEGER NOT NULL,
    author_id INTEGER NOT NULL,
    label TEXT NOT NULL,
    body TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (project_id, id),
    FOREIGN KEY (project_id, thread_id) REFERENCES threads (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, author_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

CREATE INDEX messages_by_thread ON messages (project_id, thread_id, id);

-- That a member read a message.
CREATE TABLE reads (
    project_id INTEGER NOT NULL,
    member_id INTEGER NOT NULL,
    message_id INTEGER NOT NULL,
    read_at INTEGER NOT NULL,
    PRIMARY KEY (project_id, member_id, message_id),
    FOREIGN KEY (project_id, member_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, message_id) REFERENCES messages (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

-- The append-only log. `AUTOINCREMENT` never hands out an id again, even after the newest row is
-- gone, so ids only increase. `kind` is the event's name and `payload` its fields as JSON.
CREATE TABLE events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id INTEGER NOT NULL REFERENCES projects (id) DEFERRABLE INITIALLY DEFERRED,
    kind TEXT NOT NULL,
    payload TEXT NOT NULL CHECK (json_valid(payload)),
    at INTEGER NOT NULL
) STRICT;

CREATE INDEX events_by_project ON events (project_id, id);

-- The settings document as JSON, repositories included.
CREATE TABLE settings (
    project_id INTEGER PRIMARY KEY NOT NULL REFERENCES projects (id)
        DEFERRABLE INITIALLY DEFERRED,
    document TEXT NOT NULL CHECK (json_valid(document))
) STRICT;

-- A browser session, kept as the hash of its id and bound to the token that opened it.
CREATE TABLE ui_sessions (
    hash TEXT PRIMARY KEY NOT NULL,
    project_id INTEGER NOT NULL,
    member_id INTEGER NOT NULL,
    token_hash TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    FOREIGN KEY (project_id, member_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, token_hash) REFERENCES tokens (project_id, hash)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

CREATE INDEX ui_sessions_by_expiry ON ui_sessions (expires_at);
