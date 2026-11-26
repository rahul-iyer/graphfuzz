# graphfuzz

**A differential and metamorphic fuzzer for graph query engines that speak ISO GQL (ISO/IEC 39075) or Cypher.**

graphfuzz builds random, well-typed graph queries from one AST. It prints each query in every engine's dialect
(GQL, Cypher 25 or openCypher) and runs it on every configured engine. It then reports where an engine
disagrees with the reference engine (Neo4j) or with itself. Each failing query is reduced to a minimal one
that still fails the same way.

It has already found wrong answers and crashes in Neo4j, [DuckGQL](https://github.com/rahul-iyer/duckdb-gql)
and [Ladybug](https://github.com/LadybugDB/ladybug). See [Bugs found](#bugs-found).

```
             ┌──────────── random schema + fixture (from the seed)
             ▼
  typed AST ──► render ──► GQL ─────────► DuckGQL ─┐
     │                 ├──► openCypher ──► Ladybug ─┼──► compare ──► reduce ──► report.md
     │                 └──► Cypher 25 ──► Neo4j ───┘   (reference)
     └──► metamorphic rewrites (TLP, count(*), DISTINCT, path reversal, A|A ≡ A)
```

## Quickstart

By default, graphfuzz tests DuckGQL against Neo4j.

```sh
docker compose up -d                                 # Neo4j 2026.10 on localhost:7474
cp .cargo/config.toml.example .cargo/config.toml     # set DUCKDB_LIB_DIR and DUCKDB_INCLUDE_DIR to your DuckGQL build
cargo build --release
./target/release/graphfuzz run --config config.yml   # writes runs/latest/report.md
```

**Ladybug against Neo4j (optional).** This needs no DuckGQL build. First set `enabled: true` on the `ladybug`
engine in `config.yml`. Then run:

```sh
cargo build --release --no-default-features --features ladybug   # use --features ladybug to keep DuckGQL too
./target/release/graphfuzz run --config config.yml
```

**Without Docker Compose**, start Neo4j yourself:

```sh
docker run -d --name graphfuzz-neo4j -p 7474:7474 -p 7687:7687 \
  -e NEO4J_AUTH=neo4j/graphfuzz123 neo4j:2026.10-community
```

**Check a single query** on every engine:

```sh
./target/release/graphfuzz repro --config config.yml --gql "MATCH (a)-[:KNOWS]->(b) RETURN count(*) AS c0"
```

## Bugs found

To reproduce these counts, run `graphfuzz repro --seed 1` on the declared schema, after removing `random:` from
`schema` in `config.yml`. The same failures appear on any seed, though with different counts.

| engine | query | answer | expected |
|---|---|---|---|
| Neo4j 2026.10.2 | `MATCH (a)-[:LOCATED_IN\|LOCATED_IN]->(b) RETURN count(*)` | 16 | 8, as `-[:LOCATED_IN]->` (see [neo4j#14041](https://github.com/neo4j/neo4j/issues/14041)) |
| DuckGQL | `MATCH (a)-[:KNOWS\|KNOWS]->(b) RETURN count(*)` | 40 | 20, as `-[:KNOWS]->` |
| DuckGQL | `MATCH (a:Person)-[:KNOWS]->(a) RETURN count(*)` (`--seed 11`, random schema) | 30, every KNOWS edge | 2, the edges from a node to itself |
| DuckGQL | `MATCH (n) ORDER BY n.id LIMIT 6 RETURN sum(n.id - n.population)` | 6 rows | 1 row (-74, as Neo4j) |
| Ladybug (`lbug` 0.22.3) | `RETURN count(DISTINCT NULL)` | assertion failure, `UNREACHABLE_CODE` in `types.cpp` | 0 |
| Ladybug (`lbug` 0.22.3) | `MATCH (n:Company) RETURN count(NULL)` | "Trying to a create a vector with ANY type" | 0 |
| Ladybug (`lbug` 0.22.3) | `MATCH (n:Person) WHERE CASE WHEN CASE WHEN n.age < 0 THEN FALSE ELSE 20 < n.age END THEN FALSE ELSE TRUE END RETURN count(*)` | 12 | 5: an outer `WHEN NULL` takes the ELSE branch |
| Ladybug (`lbug` 0.22.3) | `MATCH (a:Person) OPTIONAL MATCH (b:Company) WHERE a.seen IS NULL RETURN count(*)` | 12 | 27 |
| Ladybug (`lbug` 0.22.3) | `MATCH (n:Person) RETURN count(n.balance)` (a DECIMAL) | binder error: COUNT rejects DECIMAL(18, 3) | 10 |

Ladybug answers both duplicated-type queries correctly (8 and 20).

Some differences are not bugs, and the report names them on their own. One is `match_mode`. In a MATCH, Neo4j
never uses one relationship twice (DIFFERENT RELATIONSHIPS). DuckGQL and Ladybug may (REPEATABLE ELEMENTS,
the GQL default). So `MATCH (a)-[]->(b), (a)-[]->(c)` returns more rows on them. Before graphfuzz reports such
a mismatch, it re-runs the query on Neo4j with `MATCH REPEATABLE ELEMENTS`.

## How it works

- **One typed AST, many dialects.** The generator tracks the type of every variable and expression, so each
  query reads, compares, inserts and sets values of the right type. Profiles in `profiles/` say how each
  dialect spells a feature, such as quantifiers, string concatenation, LET/FILTER/NEXT as WITH, or function
  names. They also list the features a dialect lacks.
- **Queries that reach data.** Filters compare properties with values the fixture actually holds, so most
  queries bind rows instead of returning empty. The report's *Data reached* section says how many bound
  nothing.
- **A random schema per seed.** With `schema.random`, the declared node and edge types are extended with random
  types and properties, including self-loop edge types. Each type's node count and edge density is drawn too.
  `seed: random` picks a new seed every run. `--seed N` on `run`, `repro` or `generate` repeats a run exactly.
- **Oracles:**
  - **Differential:** each engine under test against the reference, each on its own share of queries.
  - **Metamorphic:** rewrites that must not change an answer: partitioning a filter three ways (TLP),
    `count(*)`, `DISTINCT`, path reversal, and `:A|A ≡ :A`. They also cover queries the reference cannot
    express.
- **Reduction.** A greedy AST reducer shrinks each failure. It keeps a reduction only if the mismatch has the
  same shape: the same side is empty, and the two row counts compare the same way. This keeps a reduction from
  sliding onto a different bug.
- **Normalized results.** Numbers are rounded to seven digits, dates printed as `YYYY-MM-DD`, and datetimes as
  UTC `YYYY-MM-DDTHH:MM:SS`. Nodes, relationships and paths are compared by their labels and properties.
- **Isolation.** Embedded engines run in crash-isolated workers, so a segfault is a finding and doesn't stop the
  run. Data changes are rolled back.

## Grammar coverage

`grammars/` holds the official ANTLR grammars:

- OpenGQL's `GQL.g4` for ISO/IEC 39075;
- openCypher 9's `Cypher.g4` (M23).

graphfuzz reads these grammars itself and tries every choice they offer:

- each alternative of each rule;
- each optional element, both present and absent;
- each repetition 0, 1 and 2 times;
- with `coverage: pairs`, each pair of alternatives.

Each choice gets the shortest well-typed sentence that makes it. graphfuzz also runs the same sentence without
that choice, as a baseline. A failure counts against the choice only when the baseline doesn't fail the same
way.

```sh
./target/release/graphfuzz sentences --config config.yml --grammar gql --count 30
./target/release/graphfuzz grammar --config config.yml    # writes grammar.md and grammar.jsonl
```

## Engines

Each engine in `config.yml` is a **connector**, which says how to reach it, plus a **dialect**, which says how
to write queries for it.

| connector | engine | data changes |
|---|---|---|
| `duckgql` | DuckGQL, through the linked libduckdb (cargo feature, on by default) | rolled back |
| `ladybug` | Ladybug, through the `lbug` crate (cargo feature `ladybug`) | rolled back |
| `neo4j_http` | Neo4j's HTTP Query API | rolled back |
| `opencypher_http` | form-encoded openCypher over HTTP, like Amazon Neptune's `/openCypher` | skipped |

To add an openCypher endpoint, add a config entry. A new dialect is a new profile file, and a new protocol is
one `Engine` implementation in `src/engine.rs`. Engines whose connector isn't compiled in are skipped, with a
note. If `password` is left out of `config.yml`, it's read from `NEO4J_PASSWORD`.

## Commands

```sh
graphfuzz run      --config config.yml [--seed N]                    # fuzz; writes report.md and results.jsonl
graphfuzz repro    --config config.yml --gql "..." [--seed N]        # one query on every engine
graphfuzz generate --config config.yml --dialect opencypher --count 20 --tags
graphfuzz features                                                   # feature and group names for config.yml
```

`config.yml` documents every setting: the seed, schema, engines and phases, the weights per feature and per
group, `focus`, `disable`, and the checks.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --no-default-features      # needs no engine
```

CI can't build DuckGQL. Instead, it runs the checks above without the embedded engines and with Ladybug. Then
it runs a short fuzzing session of Ladybug against Neo4j. See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

graphfuzz is licensed under either the [Apache License, Version 2.0](LICENSE-APACHE) or the
[MIT license](LICENSE-MIT), at your option. The bundled grammars are Apache-2.0; see [NOTICE](NOTICE).

Cypher and Neo4j are trademarks of Neo4j, Inc. graphfuzz isn't affiliated with Neo4j or the openCypher project.
