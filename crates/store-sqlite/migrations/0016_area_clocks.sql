-- When the work of each claim last showed in each area it holds: a JSON object from every area of
-- the claim's `areas` to its clock, a time in nanoseconds since the Unix epoch. An area's clock is
-- when the claim declared or gained it, moved by every later declaration of it and every edit of
-- the claim's work in it. Every area held before starts its clock when this migration runs, so no
-- area counts as idle for the time before it.
ALTER TABLE claims ADD COLUMN area_clocks TEXT NOT NULL DEFAULT '{}'
    CHECK (json_valid(area_clocks) AND json_type(area_clocks) = 'object');

UPDATE claims SET area_clocks = (
    SELECT json_group_object(held.value, CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER) * 1000000)
    FROM json_each(claims.areas) AS held
);
