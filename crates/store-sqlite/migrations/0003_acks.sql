-- How much a message asks for its readers' attention, whether it asks each recipient to
-- acknowledge it, and the acknowledgements given.
--
-- `importance` is `normal`, `high` or `urgent`; every message written before is `normal` and asks
-- for no acknowledgement. A message still never changes once written: an acknowledgement is a row
-- of its own, one per message and member, and whether a recipient has acknowledged is read from
-- these rows, never kept on the message.

ALTER TABLE messages ADD COLUMN importance TEXT NOT NULL DEFAULT 'normal'
    CHECK (importance IN ('normal', 'high', 'urgent'));
ALTER TABLE messages ADD COLUMN ack_required INTEGER NOT NULL DEFAULT 0
    CHECK (ack_required IN (0, 1));

-- The messages that ask for an acknowledgement, for the list of those a member still owes.
CREATE INDEX messages_asking_ack ON messages (project_id, created_at) WHERE ack_required = 1;

-- That `member_id` acknowledged `message_id` at `at`.
CREATE TABLE acks (
    project_id INTEGER NOT NULL,
    message_id INTEGER NOT NULL,
    member_id INTEGER NOT NULL,
    at INTEGER NOT NULL,
    PRIMARY KEY (project_id, message_id, member_id),
    FOREIGN KEY (project_id, message_id) REFERENCES messages (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, member_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

-- Who read one message, for its author.
CREATE INDEX reads_by_message ON reads (project_id, message_id);
