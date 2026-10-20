-- What reached a member while they worked, and the raised messages members wrote themselves.
--
-- A member runs several sessions, each able to put a pressing message or a slot offered to them in
-- front of its agent. The first session to claim a subject delivers it: one row per member and
-- subject, written once. `subject` is the message's id, or `offer:<slot>:<deadline in unix
-- nanoseconds>` for an offer, so a new offer after one lapsed is a subject of its own.

CREATE TABLE message_deliveries (
    project_id INTEGER NOT NULL,
    member_id INTEGER NOT NULL,
    subject TEXT NOT NULL CHECK (length(subject) BETWEEN 1 AND 200),
    session_id INTEGER NOT NULL,
    at INTEGER NOT NULL,
    PRIMARY KEY (project_id, member_id, subject),
    FOREIGN KEY (project_id, member_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, session_id) REFERENCES sessions (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

-- That `member_id` wrote the high or urgent message `message_id` at `at`, counted toward the
-- project's hourly limit. What dibs writes itself is never recorded here.
CREATE TABLE raised_messages (
    project_id INTEGER NOT NULL,
    message_id INTEGER NOT NULL,
    member_id INTEGER NOT NULL,
    at INTEGER NOT NULL,
    PRIMARY KEY (project_id, message_id),
    FOREIGN KEY (project_id, message_id) REFERENCES messages (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, member_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

-- A member's counted messages in time order, for the window of the last hour.
CREATE INDEX raised_messages_by_member ON raised_messages (project_id, member_id, at);
