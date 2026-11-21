-- When each session was ended, in nanoseconds since the Unix epoch; null while it is open. An ended
-- session stays registered, so that its label keeps its id, and is not at work: it does not count
-- as a session of its member seen lately. Registering its label again, or a heartbeat from it,
-- opens it again. Every session of before is open.
ALTER TABLE sessions ADD COLUMN ended_at INTEGER;
