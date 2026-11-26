# graphfuzz

A differential and metamorphic fuzzer for graph query engines that speak ISO GQL (ISO/IEC 39075) or Cypher.

graphfuzz generates typed graph queries from one AST, prints each in every engine's dialect (GQL, Cypher 25,
openCypher), runs them on every configured engine, and reports where an engine disagrees with the reference
engine (Neo4j by default) or with itself. Failing queries are reduced to minimal ones. It also reads the
official ANTLR grammars and tries every choice they offer.

## Quickstart

Ladybug against Neo4j, with nothing to build but graphfuzz:

```sh
docker compose up -d                 # Neo4j 2026.10 on localhost:7474
cargo build --release                # with the Ladybug connector; DuckGQL is opt-in, see below
./target/release/graphfuzz repro --config config.yml --gql "MATCH (a)-[:KNOWS]->(b) RETURN count(*) AS c0"
./target/release/graphfuzz run --config config.yml          # writes runs/latest/report.md
```

Without Docker Compose, start Neo4j with
`docker run -d --name graphfuzz-neo4j -p 7474:7474 -p 7687:7687 -e NEO4J_AUTH=neo4j/graphfuzz123 neo4j:2026.10-community`.
Engines whose connector is not compiled in are skipped, with a note.

## Bugs found

Wrong answers graphfuzz has found, each reproducible with `graphfuzz repro` on the default fixture:

| engine | query | answer | expected |
|---|---|---|---|
| Neo4j 2026.10.2 | `MATCH (a)-[:LOCATED_IN\|LOCATED_IN]->(b) RETURN count(*)` | 16 | 8, as `-[:LOCATED_IN]->` (see also [neo4j#14041](https://github.com/neo4j/neo4j/issues/14041)) |
| DuckGQL | `MATCH (a)-[:KNOWS\|KNOWS]->(b) RETURN count(*)` | 40 | 20, as `-[:KNOWS]->` |
| DuckGQL | `MATCH (n) ORDER BY n.id LIMIT 6 RETURN sum(n.id - n.population)` | 6 rows | 1 row (-74, as Neo4j): the aggregate is over the six rows |
| Ladybug (`lbug` 0.22.3) | `RETURN count(DISTINCT NULL)` | assertion failure, `UNREACHABLE_CODE` in `types.cpp` | 0 |
| Ladybug (`lbug` 0.22.3) | `MATCH (n:Company) RETURN count(NULL)` | "Trying to a create a vector with ANY type" | 0 |
| Ladybug (`lbug` 0.22.3) | `MATCH (n:Person) WHERE CASE WHEN CASE WHEN n.age < 0 THEN FALSE ELSE 20 < n.age END THEN FALSE ELSE TRUE END RETURN count(*)` | 12 | 5: an outer `WHEN NULL` takes the ELSE branch |
| Ladybug (`lbug` 0.22.3) | `MATCH (a:Person) OPTIONAL MATCH (b:Company) WHERE a.seen IS NULL RETURN count(*)` | 12 | 27: no optional match survives an `IS NULL` test on an outer variable |
| Ladybug (`lbug` 0.22.3) | `MATCH (n:Person) RETURN count(n.balance)` (a DECIMAL) | binder error: COUNT rejects DECIMAL(18, 3) | 10 |

Ladybug answers both duplicated-type queries correctly (8 and 20).

Not bugs, but differences the report names on their own (`match_mode`): Neo4j never uses one relationship
twice in a MATCH (DIFFERENT RELATIONSHIPS), while DuckGQL and Ladybug may (REPEATABLE ELEMENTS, the GQL
default), so `MATCH (a)-[]->(b), (a)-[]->(c)` has more rows on them.

## Schema and types

The `schema:` section of `config.yml` declares the graph: node and edge types with typed properties (INT64,
FLOAT64, STRING, BOOL, DATE, LOCAL DATETIME, ZONED DATETIME, DECIMAL). It becomes each engine's schema
(DuckGQL's typed graph, Ladybug's tables; Neo4j needs none) and the fixture, whose values are written in each
engine's dialect: a profile's `literals` say how (`DATE '…'` in GQL, `date('…')` in Cypher, overridable per
engine). The generator tracks the type of every variable and expression, so it reads properties of the type it
needs, compares values of one type, and inserts and sets values of each property's type. Results are
normalized across engines: numbers to seven digits, dates to `YYYY-MM-DD`, datetimes to UTC
`YYYY-MM-DDTHH:MM:SS`, and nodes, relationships, and paths to their labels and properties.

DuckGQL has no temporal literals, so its connector writes temporal fixture values with SQL into its graph
tables; queries that write temporal literals stay GQL and show the gap.

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

A grammar's `typing` makes its sentences well typed. `value_rules` are rules that stand for one value; where a
sentence tries no choice inside one, it gets a value of the type its context wants instead of the rule's
shortest derivation. `types` give that type below a rule or an alternative (`SAME` for one type shared by
comparison operands), `values` list values of each type (fixture properties and literals, in turn), and
`present` includes optional elements by default, so sentences start from `MATCH (n:Person)`.
`graphfuzz sentences --grammar G --rule R` lists a rule's alternatives and elements as these keys number them.

```sh
./target/release/graphfuzz sentences --config config.yml --grammar gql --count 30   # targets and sentences
./target/release/graphfuzz grammar --config config.yml                              # grammar.md, grammar.jsonl
```

## Setup

The embedded engines are cargo features:

| feature | default | needs |
|---|---|---|
| `ladybug` | on | nothing; the `lbug` crate builds Ladybug |
| `duckgql` | off | a build of [DuckGQL](https://github.com/rahul-iyer/duckdb-gql): copy `.cargo/config.toml.example` to `.cargo/config.toml` and point `DUCKDB_LIB_DIR` and `DUCKDB_INCLUDE_DIR` at it, then `cargo build --release --features duckgql` |

The HTTP connectors (Neo4j, openCypher endpoints) are always built. `docker compose up -d` starts Neo4j with
the password `config.yml` uses; leave `password` out of `config.yml` to read it from `NEO4J_PASSWORD`.

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

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test                      # no engine needed
```

CI runs these without the embedded engines, with Ladybug, and a short run of Ladybug against Neo4j.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your
option. The bundled grammars are Apache-2.0; see [NOTICE](NOTICE). Unless you state otherwise, any
contribution you submit for inclusion is dual licensed as above, without additional terms.

Cypher and Neo4j are trademarks of Neo4j, Inc.; graphfuzz is not affiliated with Neo4j or the openCypher
project.
