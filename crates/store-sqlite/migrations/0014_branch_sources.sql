-- Where each claim's branch was learned from, the checkouts sessions report working in, and how
-- each pool worktree's hold came to its work item.
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
-- that says nothing of where it is) and when the session last reported it. The index reads one
-- member's reports in checkout order, so the sessions that reported one checkout come together.
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

-- Whether a hold's `item_id` was learned from an edit made in its worktree (1) rather than named
-- when the worktree was taken (0). Only a take named an item before, so every hold of before reads
-- as named. A hold with no item has learned none.
ALTER TABLE worktree_holds ADD COLUMN item_learned INTEGER NOT NULL DEFAULT 0
    CHECK (item_learned IN (0, 1) AND (item_learned = 0 OR item_id IS NOT NULL));

-- The areas of a claim's `areas` it gained rather than declared: a JSON array of qualified area
-- names, each one of `areas`, added because an edit fell to the claim only as its member's target
-- claim. Every area recorded before was declared.
ALTER TABLE claims ADD COLUMN gained_areas TEXT NOT NULL DEFAULT '[]'
    CHECK (json_valid(gained_areas) AND json_type(gained_areas) = 'array');
