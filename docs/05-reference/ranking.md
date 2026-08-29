---
title: Ranking
description: How Starlet scores and orders search results — the fuzzy name matcher, the BM25 prose index, the blend, and the two-stage pipeline.
sidebar:
  label: Ranking
  icon: trending-up
---

Starlet answers every keystroke from memory and SQLite. Nothing on the search
path touches the network.

## The two signals

A repository can be worth finding for two unrelated reasons, and one index
cannot serve both.

**`owner/name` is an identifier.** People recall it partially and type it
badly: `hlxed` for `helix-editor`, `btsushi/rg` for `BurntSushi/ripgrep`.
That is a fuzzy subsequence problem, and [`nucleo`][nucleo] — the matcher
behind Helix — solves it well. It rewards matches at word boundaries, after
separators, and in camel-case humps, so an acronym beats an accidental
scattering of the same letters.

**The description and the tags are prose.** Nobody remembers that the tool
they want is called `bat`; they remember it prints files with syntax
highlighting. That is a term-frequency problem, and SQLite's FTS5 with
`bm25()` solves it.

Starlet runs both and combines them.

## The formula

For a query with free text, each candidate repository `r` gets

```
score(r) = 0.7 · fuzzy_norm(r) + 0.3 · fts_norm(r)
```

where

```
fuzzy_norm(r) = fuzzy_raw(r) / max(fuzzy_raw)      … 0 if the maximum is 0
fts_norm(r)   = fts_raw(r)   / max(fts_raw)        … 0 if the maximum is 0
```

and both maxima are taken **over the candidate set for this query**, not over
the corpus.

* `fuzzy_raw(r)` is `nucleo`'s score for the query against `r.full_name`. A
  multi-word query becomes multiple atoms that must all match.
* `fts_raw(r)` is `-bm25(repos_fts, 1.0, 5.0, 3.0, 4.0)` for the row whose
  `rowid` is `r.id`. SQLite's `bm25()` is negative and smaller-is-better, so
  the store negates it before anything else sees it.

A candidate is dropped entirely when it has neither signal: no fuzzy match on
the name **and** no full-text hit.

### Why normalise per query

The two scales have nothing in common. `nucleo` emits small unsigned integers
whose magnitude depends on the needle's length and where it landed; BM25 emits
unbounded negative reals whose magnitude depends on corpus statistics. Adding
them raw would mean the weights did nothing — whichever signal happened to be
numerically larger would win every time.

Min-max normalising inside the candidate set makes 0.7 and 0.3 mean what they
say: a perfect name match with no description hit (0.70) beats a perfect
description match with no name hit (0.30), and the crossover is exactly where
the ratio says it should be.

The cost of this choice is that scores are not comparable between queries.
Nothing needs them to be — they exist to order one list.

### Why the FTS column weights are what they are

```
full_name   1.0
description 5.0
topics      3.0
tags        4.0
```

`full_name` is weighted *down* inside FTS because the fuzzy matcher already
owns that field and carries 70 % of the final score. Weighting it heavily in
both halves would count the same evidence twice. What FTS uniquely contributes
is the prose and the tag vocabulary, so those dominate its half.

## Sorting and ties

Sort order is chosen in this precedence:

1. an explicit `sort:` prefix in the query;
2. the column header the user last clicked;
3. relevance, when the query has text;
4. most recently starred, when it does not.

Every ordering falls through to the same tie-break: **stars descending, then
`full_name` ascending**. `full_name` is unique within one account, so the
comparison is always decisive and the list never reshuffles between two renders
of the same data.

## The two stages

Ranking happens twice per keystroke.

**Stage one is synchronous.** Parse the query, apply the structured filters and
the sidebar facets in memory, fuzzy-rank what survives, hand the order to the
table. This is what the user sees within the frame. Measured median on a
5 000-repository corpus for the worst case — a single-character query, where
almost everything matches:

| Operation | Median |
| --- | --- |
| parse + fuzzy rank + sort, 5 000 repos | 0.9 ms |
| filter + rank with three clauses | 1.1 ms |
| browse ordering, empty query | 0.1 ms |

**Stage two is asynchronous.** The store is asked for BM25 relevance on the
I/O runtime; when it answers, the view re-ranks with both signals. Measured
median for the FTS query on the same corpus: **3–4 ms**, so it typically lands
within a frame or two of the keystroke.

Each query carries a revision number. A stage-two result whose revision no
longer matches the current query is discarded rather than applied, so a slow
answer can never overwrite a newer search.

The visible consequence is that a description-only match — searching `wings`
and finding `sharkdp/bat` — appears a frame after a name match would have. The
alternative was blocking the render thread on a database query, which is worse.

## The query language

```
query   := item (ws+ item)*
item    := '-'? (field ':' value) | word
field   := lang | language | tag | group | owner | user | stars | is | sort
value   := '"' … '"' | word
```

| Prefix | Matches |
| --- | --- |
| `lang:rust` | `primary_language`, or any key of the language breakdown |
| `tag:cli` | a tag from any source, or a GitHub topic |
| `group:editors` | an AI-assigned group |
| `owner:helix-editor` | the account, exactly |
| `stars:>1000` | also `>=`, `<`, `<=`, `100..500`, `1500`, and `k`/`M` suffixes |
| `is:archived` | also `is:fork`, `is:active`, `is:source` |
| `sort:stars` | also `relevance`, `name`, `recent`, `starred` |

A leading `-` negates a clause. Clauses are ANDed; sidebar facets are ORed
within a facet and ANDed across facets.

Parsing is **total**. Anything that is not a recognised `field:value` pair
becomes free text, and an incomplete prefix such as `lang:` constrains nothing.
Both matter because the parser runs on a string the user is halfway through
typing: a half-finished prefix must not blank the result list, and a pasted URL
must not become an error.

[nucleo]: https://github.com/helix-editor/nucleo
