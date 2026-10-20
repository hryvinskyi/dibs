-- Taking an area from debt work: what a claim's work is, and the areas a feature claim waits for a
-- debt claim to hand over.
--
-- `claims.kind` is `feature` or `debt`; every claim written before is a feature.

ALTER TABLE claims ADD COLUMN kind TEXT NOT NULL DEFAULT 'feature'
    CHECK (kind IN ('feature', 'debt'));

-- One row per area a claim waits for: the item of the debt claim it waits for (`from_item_id`),
-- when that claim's holder was asked to hand it over and when the unanswered request was
-- escalated, each NULL until then, and the conversation that asks for it (`request_thread_id`,
-- NULL until one was started). That conversation may be deleted later, so it is a plain id that
-- the service looks up, not a reference. An area waits for one claim of a project at a time, which
-- the key holds. The row goes with its claim: the store removes it when the claim is replaced or
-- removed. Only a feature claim waits; that rule reads the claim's kind in another table, which a
-- CHECK cannot, so the service keeps it.
CREATE TABLE claim_pending (
    project_id INTEGER NOT NULL,
    area TEXT NOT NULL,
    item_id INTEGER NOT NULL,
    from_item_id INTEGER NOT NULL CHECK (from_item_id <> item_id),
    asked_at INTEGER,
    escalated_at INTEGER CHECK (escalated_at IS NULL OR asked_at IS NOT NULL),
    request_thread_id INTEGER,
    PRIMARY KEY (project_id, area),
    FOREIGN KEY (project_id, item_id) REFERENCES claims (project_id, item_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, from_item_id) REFERENCES items (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

CREATE INDEX claim_pending_by_claim ON claim_pending (project_id, item_id);
