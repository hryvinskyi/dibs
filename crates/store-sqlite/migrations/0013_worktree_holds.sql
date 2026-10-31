-- The holds of the worktree pools of members' machines.
--
-- A pool is the member's, on one machine (`machine`, the 32 hexadecimal digits the member's dibs
-- gave it), for one checkout there (`pool`, a digest of the pool's root directory): it exists only
-- through its rows, and one where nothing is held has none. A row is one worktree lent to one
-- holder: the session `session_id` itself when `agent` is null, else the agent of that session the
-- host knows by `agent`. A worktree is held at most once per pool, and a holder holds at most one
-- worktree of a pool. `host` is the holder's machine as people read it, `item_id` the work item it
-- is held for, `last_seen` when its holder was last seen using it. The primary key keeps a pool's
-- rows together in the order of their worktrees, which is all that reading one pool, the project's
-- pools or the busy pools of every project asks of an index.
CREATE TABLE worktree_holds (
    project_id INTEGER NOT NULL,
    member_id INTEGER NOT NULL,
    machine TEXT NOT NULL CHECK (length(machine) = 32 AND machine NOT GLOB '*[^0-9a-f]*'),
    pool TEXT NOT NULL CHECK (length(pool) BETWEEN 1 AND 64 AND pool NOT GLOB '*[^0-9a-z]*'),
    worktree TEXT NOT NULL,
    session_id INTEGER NOT NULL,
    agent TEXT CHECK (agent <> ''),
    host TEXT NOT NULL,
    purpose TEXT NOT NULL,
    item_id INTEGER,
    taken_at INTEGER NOT NULL,
    last_seen INTEGER NOT NULL,
    PRIMARY KEY (project_id, member_id, machine, pool, worktree),
    FOREIGN KEY (project_id, member_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, session_id) REFERENCES sessions (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, item_id) REFERENCES items (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

-- One worktree per holder and pool. A session holding for itself has no agent, and an agent's key
-- is never empty, so the empty text stands for "the session itself": a table constraint would
-- count every null agent as distinct and let a session hold twice.
CREATE UNIQUE INDEX worktree_holds_one_per_holder
    ON worktree_holds (project_id, member_id, machine, pool, session_id, ifnull(agent, ''));
