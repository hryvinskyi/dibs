-- What dibs decides on a holder's behalf from what it stores: grants it gives, requests it will not
-- decide again, wide claims' announcements it acknowledges or stops judging, requests and
-- announcements it leaves to their holders, revokes that stick, which of two colliding claims
-- lands first, and the escalations it raises on its own. Times are nanoseconds since the Unix epoch.
--
-- `grants.decided` is who decided each grant: `holder`, the holder who gave it, or the reason dibs
-- gave it for them — `untouched` (the branch of the holder's claim changes no path of the area),
-- `in_review` (the holder's claim is in review), `idle` (the area lies idle on the holder's claim)
-- or `waited` (the request waited the project's `grant_after`, whatever the holder answered
-- meanwhile). A grant dibs gives is still the holder's grant. Every grant stored before was given
-- by its holder.
ALTER TABLE grants ADD COLUMN decided TEXT NOT NULL DEFAULT 'holder'
    CHECK (decided IN ('holder', 'untouched', 'in_review', 'idle', 'waited'));

-- `grant_asks.settled_at` is when the request was settled; null while it is not. dibs never decides
-- a settled request again. A request is settled when dibs granted it on the holder's behalf, when
-- the holder gave a grant that answers it (to the asker, in its area), when a grant from its holder
-- to its asker in its area was revoked, when the claim it was asked under has ended, or when its
-- holder holds its area through no claim at all, so that it can never wait again.
ALTER TABLE grant_asks ADD COLUMN settled_at INTEGER;

-- `grant_asks.left_to_holder` is 1 when dibs leaves the request to its holder for good and no longer
-- decides it: a revoke between its asker and its holder in its area sticks, or its holder answered
-- it in its conversation before dibs decided requests on holders' behalf. It is 0 otherwise. A
-- request left to its holder still waits for the holder's decision, and is still settled as any
-- other.
ALTER TABLE grant_asks ADD COLUMN left_to_holder INTEGER NOT NULL DEFAULT 0
    CHECK (left_to_holder IN (0, 1));

-- A request stored before was put to its holder under the rule that nothing was granted until they
-- gave it, and that they refused by answering in its conversation. One whose holder wrote a message
-- in that conversation — the thread of the message that asked — is left to the holder, so that dibs
-- does not grant what they may have refused. One they did not answer is unsettled and decided as a
-- new one is.
UPDATE grant_asks SET left_to_holder = 1
WHERE EXISTS (SELECT 1 FROM messages asking
    JOIN messages answer ON answer.project_id = asking.project_id
        AND answer.thread_id = asking.thread_id
    WHERE asking.project_id = grant_asks.project_id AND asking.id = grant_asks.message_id
        AND answer.author_id = grant_asks.holder_id);

-- The sweep reads the requests of a project dibs still decides through this partial index, which
-- settled requests and requests left to their holders do not grow.
CREATE INDEX grant_asks_undecided ON grant_asks (project_id)
    WHERE settled_at IS NULL AND left_to_holder = 0;

-- `wide_asks.dibs_acked_at` is when dibs acknowledged the announcement on its holder's behalf, and
-- `dibs_acked_because` why (`untouched`, `in_review`, `idle` or `waited`, as for a grant); both are
-- null while dibs did not, and one is never set without the other. An announcement dibs
-- acknowledged is answered, as one its holder acknowledged is, although no acknowledgement of the
-- holder's is stored. No announcement stored before was acknowledged by dibs.
ALTER TABLE wide_asks ADD COLUMN dibs_acked_at INTEGER;

ALTER TABLE wide_asks ADD COLUMN dibs_acked_because TEXT
    CHECK ((dibs_acked_because IS NULL
            OR dibs_acked_because IN ('untouched', 'in_review', 'idle', 'waited'))
        AND (dibs_acked_because IS NULL) = (dibs_acked_at IS NULL));

-- `wide_asks.settled_at` is when dibs stopped judging the announcement because it closed: its wide
-- claim ended, or its holder holds none of its areas through any claim, live or stale. Null while
-- it did not. A settled announcement stays open for its holder to acknowledge; dibs never
-- acknowledges it. An announcement is settled or acknowledged by dibs, never both: whichever comes
-- first is kept.
ALTER TABLE wide_asks ADD COLUMN settled_at INTEGER
    CHECK (settled_at IS NULL OR dibs_acked_at IS NULL);

-- `wide_asks.left_to_holder` is 1 when dibs leaves the announcement to its holder for good and no
-- longer judges it: a revoke between the wide claim's member and the holder in one of its areas
-- sticks, or its holder answered it in its conversation before dibs acknowledged announcements on
-- holders' behalf. It is 0 otherwise. One left to its holder stays open for the holder to
-- acknowledge.
ALTER TABLE wide_asks ADD COLUMN left_to_holder INTEGER NOT NULL DEFAULT 0
    CHECK (left_to_holder IN (0, 1));

-- An announcement stored before asked its holder not to acknowledge it to refuse, but to reply in
-- its conversation. One whose holder wrote a message in that conversation — the thread of the
-- message that carries it — is left to the holder; one they did not answer is judged as a new one
-- is.
UPDATE wide_asks SET left_to_holder = 1
WHERE EXISTS (SELECT 1 FROM messages sent
    JOIN messages answer ON answer.project_id = sent.project_id
        AND answer.thread_id = sent.thread_id
    WHERE sent.project_id = wide_asks.project_id AND sent.id = wide_asks.message_id
        AND answer.author_id = wide_asks.to_id);

-- Which of the two claims of a collision lands first. `order_rule` is the step of the order rule
-- that told them apart — `in_review` (one is in review, the other at work), `priority` (one item has
-- the higher priority) or `older` (one was claimed earlier) — or `tied` when no step did, and
-- `order_first` the item that lands first, set exactly when a step told them apart. Both are null
-- while the order was not worked out, and always for a moved base. That `order_first` is one of the
-- overlap's items, which are rows of `overlap_items`, is held by the code that writes it and checked
-- by the code that reads it. No overlap stored before has an order.
ALTER TABLE overlaps ADD COLUMN order_first INTEGER;

ALTER TABLE overlaps ADD COLUMN order_rule TEXT
    CHECK ((order_rule IS NULL OR order_rule IN ('in_review', 'priority', 'older', 'tied'))
        AND (order_first IS NOT NULL) = (order_rule IS NOT NULL AND order_rule <> 'tied')
        AND (kind <> 'main_moved' OR (order_rule IS NULL AND order_first IS NULL)));

-- `escalations.raised_by_dibs` is 1 when dibs raised the escalation on its own, nobody having
-- asked, as it does for two colliding claims the order rule ties: `by_id` is then the member it is
-- recorded for, one of its parties, who asked nothing. It is 0 when a member asked for it, or dibs
-- asked for a member. Every escalation stored before was asked for.
ALTER TABLE escalations ADD COLUMN raised_by_dibs INTEGER NOT NULL DEFAULT 0
    CHECK (raised_by_dibs IN (0, 1));

-- `escalations.ties` lists the pairs of colliding claims the order rule ties that dibs put to the
-- escalation's arbiters, by raising it for them or by adding them to it, each as the two work items'
-- ids, the lower first: a JSON array of two-number arrays. A pair is put to one escalation once.
-- Every escalation stored before carries none.
ALTER TABLE escalations ADD COLUMN ties TEXT NOT NULL DEFAULT '[]'
    CHECK (json_valid(ties) AND json_type(ties) = 'array');

-- The revokes that stick. A row records that a grant from the holder `holder_id` to the asker
-- `asker_id` in the area `area` (`repo:area`) was revoked at `revoked_at`, between the asker's claim
-- on `asker_item` and the holder's claim on `holder_item`, each named by when its member claimed
-- the item (`asker_claimed_at`, `holder_claimed_at`, kept by a renewal and new on a new claim), so
-- that they compare exactly with the claims' `claimed_at`. While both of those claims last, dibs
-- never grants that asker that area on that holder's behalf again; the holder still may. A row
-- outlives the claims it names, and then blocks nothing. Recording the same block again keeps the
-- first row.
CREATE TABLE grant_blocks (
    project_id INTEGER NOT NULL,
    asker_id INTEGER NOT NULL,
    asker_item INTEGER NOT NULL,
    asker_claimed_at INTEGER NOT NULL,
    holder_id INTEGER NOT NULL,
    holder_item INTEGER NOT NULL,
    holder_claimed_at INTEGER NOT NULL,
    area TEXT NOT NULL,
    revoked_at INTEGER NOT NULL,
    PRIMARY KEY (project_id, asker_id, asker_item, asker_claimed_at, holder_id, holder_item,
        holder_claimed_at, area),
    FOREIGN KEY (project_id, asker_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, asker_item) REFERENCES items (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, holder_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, holder_item) REFERENCES items (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;
