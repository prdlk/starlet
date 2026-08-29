-- Starlet's local mirror of the user's stars.
--
-- Every timestamp is RFC 3339 in UTC, stored as TEXT so the file stays
-- readable with the sqlite3 CLI. JSON columns hold shapes GitHub already
-- returns as JSON; they are never queried structurally, only round-tripped.

CREATE TABLE repos (
    id                INTEGER PRIMARY KEY,
    node_id           TEXT    NOT NULL UNIQUE,
    full_name         TEXT    NOT NULL,
    name              TEXT    NOT NULL,
    owner             TEXT    NOT NULL,
    html_url          TEXT    NOT NULL,
    description       TEXT,
    stargazers        INTEGER NOT NULL DEFAULT 0,
    last_commit_at    TEXT,
    primary_language  TEXT,
    languages_json    TEXT    NOT NULL DEFAULT '{}',
    contributors_json TEXT,
    starred_at        TEXT,
    archived          INTEGER NOT NULL DEFAULT 0,
    fork              INTEGER NOT NULL DEFAULT 0,
    topics_json       TEXT    NOT NULL DEFAULT '[]',
    updated_at        TEXT,
    synced_at         TEXT,
    -- Conditional-request support for the 24 h metadata refresh.
    etag              TEXT,
    -- README cache; `readme_fetched_at` drives the 7 day expiry.
    readme_md         TEXT,
    readme_fetched_at TEXT
);

CREATE INDEX idx_repos_full_name ON repos (full_name);
CREATE INDEX idx_repos_owner ON repos (owner);
CREATE INDEX idx_repos_language ON repos (primary_language);
CREATE INDEX idx_repos_stargazers ON repos (stargazers DESC);
CREATE INDEX idx_repos_starred_at ON repos (starred_at DESC);
CREATE INDEX idx_repos_synced_at ON repos (synced_at);

CREATE TABLE tags (
    id     INTEGER PRIMARY KEY,
    name   TEXT NOT NULL,
    -- 'github' | 'ai' | 'user'
    source TEXT NOT NULL,
    UNIQUE (name, source)
);

CREATE TABLE repo_tags (
    repo_id    INTEGER NOT NULL REFERENCES repos (id) ON DELETE CASCADE,
    tag_id     INTEGER NOT NULL REFERENCES tags (id) ON DELETE CASCADE,
    confidence REAL    NOT NULL DEFAULT 1.0,
    PRIMARY KEY (repo_id, tag_id)
);

CREATE INDEX idx_repo_tags_tag ON repo_tags (tag_id);

CREATE TABLE "groups" (
    id      INTEGER PRIMARY KEY,
    name    TEXT NOT NULL UNIQUE,
    summary TEXT NOT NULL DEFAULT '',
    source  TEXT NOT NULL
);

CREATE TABLE repo_groups (
    repo_id  INTEGER NOT NULL REFERENCES repos (id) ON DELETE CASCADE,
    group_id INTEGER NOT NULL REFERENCES "groups" (id) ON DELETE CASCADE,
    PRIMARY KEY (repo_id, group_id)
);

CREATE INDEX idx_repo_groups_group ON repo_groups (group_id);

-- Key/value scratchpad. `sync.*` keys belong to the sync engine,
-- `ui.*` keys to persisted interface state such as column widths.
CREATE TABLE sync_state (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE ai_runs (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    provider       TEXT NOT NULL,
    model          TEXT NOT NULL,
    started_at     TEXT NOT NULL,
    finished_at    TEXT,
    repos_count    INTEGER NOT NULL DEFAULT 0,
    cost_estimate  REAL    NOT NULL DEFAULT 0.0
);

-- Full-text index. Not an external-content table: the `tags` column is
-- assembled from a join, so FTS5 has to own its own copy of the text.
-- `rowid` is always `repos.id`.
CREATE VIRTUAL TABLE repos_fts USING fts5 (
    full_name,
    description,
    topics,
    tags,
    tokenize = 'unicode61 remove_diacritics 2'
);

CREATE TRIGGER repos_fts_insert AFTER INSERT ON repos BEGIN
    INSERT INTO repos_fts (rowid, full_name, description, topics, tags)
    VALUES (new.id, new.full_name, coalesce(new.description, ''), new.topics_json, '');
END;

CREATE TRIGGER repos_fts_delete AFTER DELETE ON repos BEGIN
    DELETE FROM repos_fts WHERE rowid = old.id;
END;

-- Leaves `tags` alone: repo metadata refreshes must not drop tag text.
CREATE TRIGGER repos_fts_update AFTER UPDATE ON repos BEGIN
    UPDATE repos_fts
       SET full_name   = new.full_name,
           description = coalesce(new.description, ''),
           topics      = new.topics_json
     WHERE rowid = new.id;
END;

CREATE TRIGGER repo_tags_fts_insert AFTER INSERT ON repo_tags BEGIN
    UPDATE repos_fts
       SET tags = (SELECT coalesce(group_concat(t.name, ' '), '')
                     FROM repo_tags rt JOIN tags t ON t.id = rt.tag_id
                    WHERE rt.repo_id = new.repo_id)
     WHERE rowid = new.repo_id;
END;

CREATE TRIGGER repo_tags_fts_delete AFTER DELETE ON repo_tags BEGIN
    UPDATE repos_fts
       SET tags = (SELECT coalesce(group_concat(t.name, ' '), '')
                     FROM repo_tags rt JOIN tags t ON t.id = rt.tag_id
                    WHERE rt.repo_id = old.repo_id)
     WHERE rowid = old.repo_id;
END;
