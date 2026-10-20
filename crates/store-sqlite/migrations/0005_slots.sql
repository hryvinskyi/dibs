-- Slots: who holds which shared action now, who waits for it, and to whom it is on offer.
--
-- A slot is declared in the project's settings; these tables keep only what members did with it.
-- A hold is the member's, at most one per slot. How it stays alive is `liveness`: `session` (the
-- member's heartbeats), `run` (the heartbeats of the run `run_id`), or `until_run` (the forge's
-- run of `workflow` for the commit `sha`). `stalled_at` is set while a run's hold goes without
-- heartbeats. A waiter is a member's place in a slot group's queue, ordered by `since`, then `id`.
-- An offer is a freed slot reserved for the member first in its queue until `until`.

CREATE TABLE slot_holds (
    project_id INTEGER NOT NULL,
    slot TEXT NOT NULL,
    member_id INTEGER NOT NULL,
    session_id INTEGER NOT NULL,
    host TEXT NOT NULL,
    purpose TEXT NOT NULL,
    item_id INTEGER,
    liveness TEXT NOT NULL CHECK (liveness IN ('session', 'run', 'until_run')),
    run_id INTEGER,
    workflow TEXT,
    sha TEXT,
    taken_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    last_seen INTEGER NOT NULL,
    stalled_at INTEGER,
    PRIMARY KEY (project_id, slot),
    CHECK ((liveness = 'run') = (run_id IS NOT NULL)),
    CHECK ((liveness = 'until_run') = (workflow IS NOT NULL)),
    CHECK ((workflow IS NULL) = (sha IS NULL)),
    CHECK (stalled_at IS NULL OR liveness = 'run'),
    FOREIGN KEY (project_id, member_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, session_id) REFERENCES sessions (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, item_id) REFERENCES items (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

CREATE TABLE slot_waiters (
    project_id INTEGER NOT NULL,
    id INTEGER NOT NULL,
    slot TEXT NOT NULL,
    member_id INTEGER NOT NULL,
    purpose TEXT NOT NULL,
    since INTEGER NOT NULL,
    last_seen INTEGER NOT NULL,
    PRIMARY KEY (project_id, id),
    UNIQUE (project_id, member_id, slot),
    FOREIGN KEY (project_id, member_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

-- The queue in order.
CREATE INDEX slot_waiters_in_line ON slot_waiters (project_id, since, id);

CREATE TABLE slot_offers (
    project_id INTEGER NOT NULL,
    slot TEXT NOT NULL,
    member_id INTEGER NOT NULL,
    purpose TEXT NOT NULL,
    until INTEGER NOT NULL,
    PRIMARY KEY (project_id, slot),
    FOREIGN KEY (project_id, member_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;
