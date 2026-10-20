-- Review: a claim whose work waits in pull requests, and when each integration last called.
--
-- A claim is in review when it records pull requests: `review_prs` is a non-empty JSON array of
-- `<owner>/<name>#<number>` texts, `review_since` the time it went into review, and `review_more`
-- whether more pull requests of the item follow them (0 or 1). A claim its holder works on has all
-- three NULL, as every claim written before has.

ALTER TABLE claims ADD COLUMN review_prs TEXT
    CHECK (review_prs IS NULL OR (json_valid(review_prs) AND json_type(review_prs) = 'array'
        AND json_array_length(review_prs) > 0));
ALTER TABLE claims ADD COLUMN review_since INTEGER
    CHECK ((review_since IS NULL) = (review_prs IS NULL));
ALTER TABLE claims ADD COLUMN review_more INTEGER
    CHECK ((review_more IS NULL) = (review_prs IS NULL) AND (review_more IS NULL OR review_more IN (0, 1)));

-- When the member `member_id`, an integration, last called the service.
CREATE TABLE member_calls (
    project_id INTEGER NOT NULL,
    member_id INTEGER NOT NULL,
    at INTEGER NOT NULL,
    PRIMARY KEY (project_id, member_id),
    FOREIGN KEY (project_id, member_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;
