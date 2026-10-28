-- The grants members asked of the holders of areas.
--
-- That the message `message_id` asked the holder `holder_id` for a grant to `asker_id` of edits in
-- the area `repo:area` for the asker's work item `item_id`. Whether the holder answered is read
-- from the messages of the conversation it started; whether it still waits for their decision,
-- from the claims and the grants.
CREATE TABLE grant_asks (
    project_id INTEGER NOT NULL,
    message_id INTEGER NOT NULL,
    asker_id INTEGER NOT NULL,
    item_id INTEGER NOT NULL,
    repo TEXT NOT NULL,
    area TEXT NOT NULL,
    holder_id INTEGER NOT NULL,
    PRIMARY KEY (project_id, message_id),
    FOREIGN KEY (project_id, message_id) REFERENCES messages (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, asker_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, item_id) REFERENCES items (project_id, id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, holder_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

-- The requests put to one holder.
CREATE INDEX grant_asks_by_holder ON grant_asks (project_id, holder_id);
