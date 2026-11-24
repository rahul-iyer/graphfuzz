# graphfuzz

Generates graph queries from one AST, prints each in every engine's dialect, runs them on every configured
engine, and reports where an engine disagrees with the reference engine (Neo4j by default) or with itself.
Failing queries are reduced to minimal ones.

## Engines

An engine in `config.yml` is a **connector** (how to reach it) and a **dialect** (how to write queries for it).

Connectors:

| connector | engine | data modification |
|---|---|---|
| `duckgql` | DuckGQL through the linked libduckdb, in a crash-isolated worker | rolled back |
| `ladybug` | Ladybug through the `lbug` crate, in a crash-isolated worker | rolled back |
| `neo4j_http` | Neo4j's HTTP Query API | rolled back |
| `opencypher_http` | form-encoded openCypher over HTTP, as Amazon Neptune's `/openCypher` | skipped |

Dialects are profiles in `profiles/`: `gql`, `cypher25`, and `opencypher`. A profile lists the features its
language lacks (by the names `graphfuzz features` prints) and how it spells the rest: quantifiers, string
concatenation, LET/FILTER/NEXT as WITH, function names, and so on. An engine adjusts its profile with
`supported`, `unsupported`, and `syntax`, so a new openCypher endpoint is a config entry, and a new dialect is
a profile file. A new protocol is one `Engine` implementation in `src/engine.rs`.

Each case is run on the reference and on every engine whose dialect can express it; queries the reference
cannot express are checked with rewrites that must not change an answer (partitioning a filter, reversing
paths, `count(*)`, `DISTINCT`) on engines with `metamorphic: true`.

## Grammars

`grammars/` holds the official grammars: OpenGQL's `GQL.g4` for ISO/IEC 39075 (as DuckGQL vendors it) and
openCypher 9's `Cypher.g4` (milestone M23, from artifacts.opencypher.org). graphfuzz reads ANTLR 4 grammars
itself and tries every choice they offer: each alternative of each rule, each optional element present and
absent, each repetition 0, 1, and 2 times, and (`coverage: pairs`) each alternative of every rule an
alternative uses. Each choice gets the shortest sentence that makes it; a failure counts against the choice
only when the sentence without it (its baseline) does not fail the same way. Sentences run in rolled-back
transactions, the fixture is checked for drift, and engines that answer the same sentence are compared.

A grammar in `config.yml` names its file, start rule, engines, a vocabulary (fixture names, numbers, and
strings in place of derived identifiers and literals), and `prefer`, the alternatives to take when no choice
is forced, so sentences start from `MATCH ... RETURN` rather than the shortest statement.

```sh
./target/release/graphfuzz sentences --config config.yml --grammar gql --count 30   # targets and sentences
./target/release/graphfuzz grammar --config config.yml                              # grammar.md, grammar.jsonl
```

## Setup

- DuckGQL: `.cargo/config.toml` links the DuckGQL build's `libduckdb` (`DUCKDB_LIB_DIR`).
- Neo4j 2026.10, for instance:
  `docker run -d --name graphfuzz-neo4j -p 7474:7474 -p 7687:7687 -e NEO4J_AUTH=neo4j/graphfuzz123 neo4j:2026.10-community`

## Use

```sh
cargo build --release
./target/release/graphfuzz run --config config.yml          # writes report.md and results.jsonl
./target/release/graphfuzz generate --config config.yml --dialect opencypher --count 20 --tags
./target/release/graphfuzz repro --config config.yml --gql "MATCH (n:Person) RETURN count(*) AS c"
./target/release/graphfuzz features                         # feature and group names for config.yml
```

`config.yml` documents every setting: seed, engines, phases, per-feature and per-group weights, `focus`,
`disable`, and the checks.
