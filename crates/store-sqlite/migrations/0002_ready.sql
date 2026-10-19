-- What an importer reports about a work item, and the work graph between items.
--
-- `priority` is 0 to 4, lower is sooner. `areas_hint` is a JSON array of qualified area names the
-- work will probably touch. `source_status` is the outside tracker's own state of the item, kept as
-- written. `closed` says the tracker closed it; it never ends a claim. `source` names the importer's
-- source that lists the item, if any.
--
-- A `gh:` reference is kept in one form from here on: the whole value in lower case, since GitHub's
-- names are case-insensitive. The references stored before are brought to that form here. Two
-- items of one project whose references differ only in case are two records of one ticket: the
-- migration fails naming both rather than merging them, and so it does for a `gh:` reference that
-- is not `owner/repo#number`, which could not be read any more. Either is fixed by hand, as the
-- operations guide describes, before the server is started again.

ALTER TABLE items ADD COLUMN priority INTEGER NOT NULL DEFAULT 2 CHECK (priority BETWEEN 0 AND 4);
ALTER TABLE items ADD COLUMN areas_hint TEXT NOT NULL DEFAULT '[]'
    CHECK (json_valid(areas_hint) AND json_type(areas_hint) = 'array');
ALTER TABLE items ADD COLUMN source_status TEXT;
ALTER TABLE items ADD COLUMN closed INTEGER NOT NULL DEFAULT 0 CHECK (closed IN (0, 1));
ALTER TABLE items ADD COLUMN source TEXT;

-- The open items of a source, for a batch that read the whole source.
CREATE INDEX items_by_source ON items (project_id, source);

-- `item_id` is blocked by `blocked_by_id`. `source` is who recorded the edge: `manual` or
-- `imported`. A pair has one edge.
CREATE TABLE dependencies (
    project_id INTEGER NOT NULL,
    item_id INTEGER NOT NULL,
    blocked_by_id INTEGER NOT NULL,
    source TEXT NOT NULL,
    PRIMARY KEY (project_id, item_id, blocked_by_id),
    CHECK (item_id <> blocked_by_id),
    FOREIGN KEY (project_id, item_id) REFERENCES items (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, blocked_by_id) REFERENCES items (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

-- What closing an item unblocks.
CREATE INDEX dependencies_by_blocker ON dependencies (project_id, blocked_by_id);

-- A row inserted here fails the migration with its text.
CREATE TEMP TABLE gh_refs_refused (reason TEXT NOT NULL);
CREATE TEMP TRIGGER gh_refs_refused_abort BEFORE INSERT ON gh_refs_refused
BEGIN
    SELECT RAISE(ABORT, NEW.reason);
END;

INSERT INTO gh_refs_refused (reason)
SELECT 'the work items ' || a.external_ref || ' and ' || b.external_ref
    || ' name one ticket in two spellings; merge them by hand before upgrading'
FROM items a
JOIN items b ON b.project_id = a.project_id AND b.id > a.id
WHERE substr(a.external_ref, 1, 3) = 'gh:'
    AND substr(b.external_ref, 1, 3) = 'gh:'
    AND lower(a.external_ref) = lower(b.external_ref)
LIMIT 1;

INSERT INTO gh_refs_refused (reason)
SELECT 'the work item ' || external_ref
    || ' has a gh reference that is not owner/repo#number; correct it by hand before upgrading'
FROM (
    SELECT external_ref,
        repo,
        substr(repo, 1, instr(repo, '/') - 1) AS owner,
        substr(repo, instr(repo, '/') + 1) AS name,
        number
    FROM (
        SELECT external_ref,
            CASE WHEN instr(value, '#') > 0
                THEN substr(value, 1, instr(value, '#') - 1) ELSE '' END AS repo,
            CASE WHEN instr(value, '#') > 0
                THEN substr(value, instr(value, '#') + 1) ELSE '' END AS number
        FROM (
            SELECT external_ref, substr(external_ref, 4) AS value
            FROM items
            WHERE substr(external_ref, 1, 3) = 'gh:'
        )
    )
)
WHERE NOT (
    instr(repo, '/') > 0
    AND owner <> '' AND owner NOT GLOB '*[^A-Za-z0-9._-]*'
    AND name <> '' AND name NOT GLOB '*[^A-Za-z0-9._-]*'
    AND number <> '' AND number NOT GLOB '*[^0-9]*'
)
LIMIT 1;

DROP TRIGGER gh_refs_refused_abort;
DROP TABLE gh_refs_refused;

UPDATE items SET external_ref = lower(external_ref) WHERE substr(external_ref, 1, 3) = 'gh:';
