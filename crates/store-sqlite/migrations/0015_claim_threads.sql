-- The conversation dibs keeps with the holder of each claim about that claim.
--
-- A row links one conversation (`thread_id`) to one claim, named as its member took it: the item
-- (`item_id`), the member (`member_id`) and when the member first claimed the item (`claimed_at`,
-- kept by a renewal, new on a take-over or a new claim). A claim has at most one conversation and a
-- conversation is about at most one claim. The row names no row of `claims`: an ending removes the
-- claim before its holder is told, and a take-over replaces it, so the link outlives the claim it
-- names. It goes with its conversation, which removes it before the thread itself.
--
-- `base_moved_told_at` is when dibs last told the holder, in that conversation, that the base moved
-- under the claim's work; null while it never did. The unique index finds a claim's conversation.
-- Conversations started before have no row and stay as they are.
CREATE TABLE claim_threads (
    project_id INTEGER NOT NULL,
    thread_id INTEGER NOT NULL,
    item_id INTEGER NOT NULL,
    member_id INTEGER NOT NULL,
    claimed_at INTEGER NOT NULL,
    base_moved_told_at INTEGER,
    PRIMARY KEY (project_id, thread_id),
    FOREIGN KEY (project_id, thread_id) REFERENCES threads (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, item_id) REFERENCES items (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, member_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

CREATE UNIQUE INDEX claim_threads_one_per_claim
    ON claim_threads (project_id, item_id, member_id, claimed_at);
