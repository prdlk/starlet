# 6. SQLite: WAL, an owned FTS5 table, and three separate tag sources

Status: accepted

## Context

The store has to serve a full in-memory load at startup, a full-text query per
keystroke, and a background sync writing at the same time. It also has to make
"never overwrite a user tag with an AI tag" true.

## Decision

**WAL with `synchronous = NORMAL`.** Readers do not block the writer. A crash
can lose the tail of the last transaction, which for a rebuildable mirror of
GitHub is not worth an fsync per commit.

**A content-owning FTS5 table, not an external-content one.** `repos_fts`
indexes `full_name`, `description`, `topics`, and `tags`. The tag column is
assembled from a join, so an external-content table pointed at `repos` could
not produce it. `rowid` is always `repos.id`. Five triggers keep it in sync:
insert, update, and delete on `repos`, and insert and delete on `repo_tags`.
The update trigger deliberately leaves the `tags` column alone so a metadata
refresh cannot erase tag text.

**Three tag sources in the schema, not in application logic.** `tags` is unique
on `(name, source)`, and each source is replaced by its own owner: GitHub topics
on every sync, AI tags wholesale per analysis run, user tags only by the user.
An AI tag whose name already exists as a user tag on that repository is dropped
rather than stored.

**Upserts preserve what the caller cannot know.** A star-listing refresh has no
contributors, no README, and no language bytes, so the `ON CONFLICT` clause
coalesces those columns instead of overwriting them, and an empty language map
is treated as "no information" rather than "no languages".

## Consequences

* FTS5 must be compiled into the bundled SQLite. A test asserts `repos_fts`
  exists so the build fails loudly rather than degrading search silently.
* The tag-source rule is a property of the schema and the DAO, covered by
  `ai_tags_never_displace_user_tags` and `promoting_an_ai_tag_makes_it_a_user_tag`.
* Two columns are not in the original schema sketch: `etag`, required by the
  conditional-request refresh, and `readme_md` / `readme_fetched_at`, required
  by the seven-day README cache.
* `sync_state` carries two namespaces, `sync.*` and `ui.*`. Column widths and
  the appearance choice live in `ui.*`.
