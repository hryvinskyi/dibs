-- Where each claim's branch was learned from, and the checkouts sessions report working in.
--
-- `claims.branches` is a JSON object from a repository id to the branch the claim's work is on
-- there and where it was learned from: `{"branch": …, "source": …}`, `source` being
-- `pull_request` (with `pr`, the pull request, `owner/repo#n`), `worktree` (with `machine`, `pool`
-- and `worktree`, the pool worktree), `named` or `checkout`. Every branch recorded before is
-- forgotten, and so is all the radar derived from those branches: the footprints with their paths,
-- and the overlaps with the items they name. The log and the conversations stay.

DELETE FROM overlap_items;
DELETE FROM overlaps;
DELETE FROM footprint_paths;
DELETE FROM footprints;
UPDATE claims SET branches = '{}';

-- One row per session and checkout it reported working in: the member the session is of, the
-- checkout (`checkout`, the 32 lowercase hexadecimal characters its client names it by, a digest
-- that says nothing of where it is) and when the session last reported it. Rows of one member are
-- found by checkout through the index, so the sessions that share a checkout are read without
-- reading the others.
CREATE TABLE session_checkouts (
    project_id INTEGER NOT NULL,
    session_id INTEGER NOT NULL,
    member_id INTEGER NOT NULL,
    checkout TEXT NOT NULL
        CHECK (length(checkout) = 32 AND checkout NOT GLOB '*[^0-9a-f]*'),
    reported_at INTEGER NOT NULL,
    PRIMARY KEY (project_id, session_id, checkout),
    FOREIGN KEY (project_id, session_id) REFERENCES sessions (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, member_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

CREATE INDEX session_checkouts_by_checkout
    ON session_checkouts (project_id, member_id, checkout);
