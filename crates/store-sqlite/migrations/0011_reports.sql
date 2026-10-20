-- What members' machines report about the guard, and what integrations last said of each pull
-- request.
--
-- One row per member, machine and day of what that machine counted: `machine` is the id the
-- member's dibs gave the machine once (32 hexadecimal digits), so a renamed machine keeps its row
-- and two machines of one name keep two; `host` is the name the machine last reported; `day` is
-- `YYYY-MM-DD` in UTC, `counts` the kinds counted as a JSON object of numbers (`{"held_by": 2}`),
-- `durations` how long hooks took as a JSON array, one entry per agent and hook event. A resent
-- report replaces its row. Nothing in it names a path, a command or a work item.
CREATE TABLE hook_reports (
    project_id INTEGER NOT NULL,
    day TEXT NOT NULL CHECK (day GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
    member_id INTEGER NOT NULL,
    machine TEXT NOT NULL CHECK (length(machine) = 32 AND machine NOT GLOB '*[^0-9a-f]*'),
    host TEXT NOT NULL,
    counts TEXT NOT NULL CHECK (json_valid(counts) AND json_type(counts) = 'object'),
    durations TEXT NOT NULL CHECK (json_valid(durations) AND json_type(durations) = 'array'),
    PRIMARY KEY (project_id, day, member_id, machine),
    FOREIGN KEY (project_id, member_id) REFERENCES members (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

-- Whether a pull request merged cleanly into its base when an integration last reported it, and
-- when that was; the retention removes what was last reported before the log keeps.
CREATE TABLE pr_states (
    project_id INTEGER NOT NULL REFERENCES projects (id) DEFERRABLE INITIALLY DEFERRED,
    pr TEXT NOT NULL,
    mergeable INTEGER NOT NULL CHECK (mergeable IN (0, 1)),
    reported_at INTEGER NOT NULL,
    PRIMARY KEY (project_id, pr)
) STRICT;

CREATE INDEX pr_states_by_time ON pr_states (project_id, reported_at);
