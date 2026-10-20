-- The branch overlap radar: the branch each claim's work is on in each repository, what each of
-- those branches changes against its base, and the overlaps found between claims.
--
-- `claims.branches` is a JSON object from a repository id to the branch the claim's work is on
-- there. Every claim written before records none.

ALTER TABLE claims ADD COLUMN branches TEXT NOT NULL DEFAULT '{}'
    CHECK (json_valid(branches) AND json_type(branches) = 'object');

-- One footprint per claim and repository: the branch, what its paths are measured from — the base
-- branch, the merge base of the two and that commit's time — whether the forge gave fewer paths
-- than the branch changes, how many paths it holds, and what the radar reads of them under the
-- settings (`digest`, a JSON object of the areas they lie in, whether one lies in no area, and the
-- contract paths). It refers to the item rather than the claim: it is removed explicitly when the
-- claim ends.
CREATE TABLE footprints (
    project_id INTEGER NOT NULL,
    item_id INTEGER NOT NULL,
    repo TEXT NOT NULL,
    branch TEXT NOT NULL,
    base_branch TEXT NOT NULL,
    base_sha TEXT NOT NULL,
    base_at INTEGER NOT NULL,
    truncated INTEGER NOT NULL CHECK (truncated IN (0, 1)),
    path_count INTEGER NOT NULL CHECK (path_count >= 0),
    digest TEXT NOT NULL CHECK (json_valid(digest) AND json_type(digest) = 'object'),
    PRIMARY KEY (project_id, item_id, repo),
    FOREIGN KEY (project_id, item_id) REFERENCES items (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT;

-- A footprint's paths, one row each, kept in key order without a separate row id, and found by
-- path through the index, so the footprints that hold a path are read without reading the others.
CREATE TABLE footprint_paths (
    project_id INTEGER NOT NULL,
    item_id INTEGER NOT NULL,
    repo TEXT NOT NULL,
    path TEXT NOT NULL,
    PRIMARY KEY (project_id, item_id, repo, path),
    FOREIGN KEY (project_id, item_id, repo) REFERENCES footprints (project_id, item_id, repo)
        DEFERRABLE INITIALLY DEFERRED
) STRICT, WITHOUT ROWID;

CREATE INDEX footprint_paths_by_path ON footprint_paths (project_id, repo, path, item_id);

-- An overlap found, under the text of its key (`same_file 3 7`, `main_moved 3 acme/api#12`): its
-- kind, the merged pull request of a moved base and when it was merged, and the paths it is on (a
-- JSON array of `repo:path` texts). The items it names are rows of `overlap_items`.
CREATE TABLE overlaps (
    project_id INTEGER NOT NULL,
    overlap_key TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('same_file', 'contract', 'main_moved')),
    pr TEXT,
    merged_at INTEGER CHECK ((merged_at IS NULL) = (pr IS NULL)),
    paths TEXT NOT NULL CHECK (json_valid(paths) AND json_type(paths) = 'array'
        AND json_array_length(paths) > 0),
    PRIMARY KEY (project_id, overlap_key)
) STRICT;

CREATE TABLE overlap_items (
    project_id INTEGER NOT NULL,
    overlap_key TEXT NOT NULL,
    item_id INTEGER NOT NULL,
    PRIMARY KEY (project_id, overlap_key, item_id),
    FOREIGN KEY (project_id, overlap_key) REFERENCES overlaps (project_id, overlap_key)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (project_id, item_id) REFERENCES items (project_id, id)
        DEFERRABLE INITIALLY DEFERRED
) STRICT, WITHOUT ROWID;

CREATE INDEX overlap_items_by_item ON overlap_items (project_id, item_id);
