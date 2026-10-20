-- People leaving, what dibs writes itself, escalations to the arbiters, and how long records are
-- kept.
--
-- A member removed from a project keeps their row, so every record that names them still reads,
-- with the time they were removed in `removed_at`.
--
-- A thread or a message dibs writes itself, in the project's name, has no member as its author:
-- `author_id` is NULL. Threads and messages are rebuilt for that, keeping every row and id; their
-- rows and the rows that refer to them are set aside first and put back after, so no reference is
-- ever left pointing at nothing. Every thread and message written before keeps its author.
--
-- Messages are still never changed. They are now removed: with their thread, when the thread has
-- been quiet past its date, or when an admin deletes the thread; and with them everything that
-- refers to them.

ALTER TABLE members ADD COLUMN removed_at INTEGER;

CREATE TEMP TABLE kept_recipients AS SELECT * FROM thread_recipients;
CREATE TEMP TABLE kept_messages AS SELECT * FROM messages;
CREATE TEMP TABLE kept_reads AS SELECT * FROM reads;
CREATE TEMP TABLE kept_acks AS SELECT * FROM acks;
CREATE TEMP TABLE kept_wide_asks AS SELECT * FROM wide_asks;
CREATE TEMP TABLE kept_raised AS SELECT * FROM raised_messages;

DELETE FROM raised_messages;
DELETE FROM wide_asks;
DELETE FROM acks;
DELETE FROM reads;
DELETE FROM messages;
DELETE FROM thread_recipients;

-- A conversation, addressed to the whole project (`to_project`) or to the members listed in
-- `thread_recipients`. `author_id` is the member who started it, or NULL for dibs.
CREATE TABLE threads_0007 (
    project_id INTEGER NOT NULL,
    id INTEGER NOT NULL,
    subject TEXT NOT NULL,
    item_id INTEGER,
    author_id INTEGER,
    to_project INTEGER NOT NULL CHECK (to_project IN (0, 1)),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (project_id, id),
    FOREIGN KEY (project_id, item_id) REFERENCES items (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, author_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

INSERT INTO threads_0007 (project_id, id, subject, item_id, author_id, to_project, created_at)
SELECT project_id, id, subject, item_id, author_id, to_project, created_at FROM threads;
DROP TABLE threads;
ALTER TABLE threads_0007 RENAME TO threads;
CREATE INDEX threads_by_item ON threads (project_id, item_id);

-- A message. Nothing updates one. `author_id` is the member who wrote it, or NULL for dibs.
CREATE TABLE messages_0007 (
    project_id INTEGER NOT NULL,
    id INTEGER NOT NULL,
    thread_id INTEGER NOT NULL,
    author_id INTEGER,
    label TEXT NOT NULL,
    body TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    importance TEXT NOT NULL DEFAULT 'normal'
        CHECK (importance IN ('normal', 'high', 'urgent')),
    ack_required INTEGER NOT NULL DEFAULT 0 CHECK (ack_required IN (0, 1)),
    PRIMARY KEY (project_id, id),
    FOREIGN KEY (project_id, thread_id) REFERENCES threads (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, author_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

DROP TABLE messages;
ALTER TABLE messages_0007 RENAME TO messages;
CREATE INDEX messages_by_thread ON messages (project_id, thread_id, id);
CREATE INDEX messages_asking_ack ON messages (project_id, created_at) WHERE ack_required = 1;

INSERT INTO thread_recipients SELECT * FROM kept_recipients;
INSERT INTO messages (project_id, id, thread_id, author_id, label, body, created_at, importance,
    ack_required)
SELECT project_id, id, thread_id, author_id, label, body, created_at, importance, ack_required
FROM kept_messages;
INSERT INTO reads SELECT * FROM kept_reads;
INSERT INTO acks SELECT * FROM kept_acks;
INSERT INTO wide_asks SELECT * FROM kept_wide_asks;
INSERT INTO raised_messages SELECT * FROM kept_raised;

DROP TABLE kept_recipients;
DROP TABLE kept_messages;
DROP TABLE kept_reads;
DROP TABLE kept_acks;
DROP TABLE kept_wide_asks;
DROP TABLE kept_raised;

-- That the message `message_id`, which dibs wrote for `by_id`, escalated `subject` (JSON: one of
-- `{"item": n}`, `{"slot": "name"}`, `{"thread": n}`) to the arbiters `arbiters` (a JSON array of
-- member ids). It stands until one of them acknowledges the message, which is read from `acks`.
CREATE TABLE escalations (
    project_id INTEGER NOT NULL,
    message_id INTEGER NOT NULL,
    thread_id INTEGER NOT NULL,
    subject TEXT NOT NULL CHECK (json_valid(subject) AND json_type(subject) = 'object'),
    by_id INTEGER NOT NULL,
    arbiters TEXT NOT NULL CHECK (json_valid(arbiters) AND json_type(arbiters) = 'array'),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (project_id, message_id),
    FOREIGN KEY (project_id, message_id) REFERENCES messages (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, thread_id) REFERENCES threads (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, by_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

CREATE INDEX escalations_by_time ON escalations (project_id, created_at);

-- The newest event id the retention removed from a project's log: a reader whose cursor is behind
-- it missed events that cannot be read any more.
CREATE TABLE events_pruned (
    project_id INTEGER PRIMARY KEY NOT NULL REFERENCES projects (id)
        DEFERRABLE INITIALLY DEFERRED,
    through INTEGER NOT NULL
) STRICT;

-- The events of a project by time, for removing those past their date.
CREATE INDEX events_by_time ON events (project_id, at);

-- A thread's newest message, for finding the quiet ones.
CREATE INDEX messages_by_thread_time ON messages (project_id, thread_id, created_at);

-- The deliveries of one message, whoever received it, for removing them with their thread.
CREATE INDEX message_deliveries_by_subject ON message_deliveries (project_id, subject);
