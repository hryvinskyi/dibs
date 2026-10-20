-- Wide claims: a claim may name, beside the areas it holds, a scope of areas it announces a change
-- over and holds none of, with the reason it gives; and the announcement each holder of one of
-- those areas is sent.
--
-- Every claim written before is an ordinary claim: both new columns stay NULL. A wide claim has
-- both: `wide_areas` a non-empty JSON array of `repo:area` names, `wide_reason` the text.

ALTER TABLE claims ADD COLUMN wide_areas TEXT
    CHECK (wide_areas IS NULL OR (json_valid(wide_areas) AND json_type(wide_areas) = 'array'
        AND json_array_length(wide_areas) > 0));
ALTER TABLE claims ADD COLUMN wide_reason TEXT
    CHECK ((wide_reason IS NULL) = (wide_areas IS NULL));

-- That the message `message_id` announced the wide claim of `from_id` on `item_id` to the holder
-- `to_id`, about the areas `areas` (a JSON array of `repo:area` names). Whether the holder answered
-- is read from `acks`.
CREATE TABLE wide_asks (
    project_id INTEGER NOT NULL,
    message_id INTEGER NOT NULL,
    item_id INTEGER NOT NULL,
    from_id INTEGER NOT NULL,
    to_id INTEGER NOT NULL,
    areas TEXT NOT NULL CHECK (json_valid(areas) AND json_type(areas) = 'array'),
    PRIMARY KEY (project_id, message_id),
    FOREIGN KEY (project_id, message_id) REFERENCES messages (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, item_id) REFERENCES items (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, from_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, to_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

-- The announcements of one wide claim.
CREATE INDEX wide_asks_by_item ON wide_asks (project_id, item_id);
