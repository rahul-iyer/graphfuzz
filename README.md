# graphfuzz

Generates graph queries from one AST as ISO GQL and as Cypher 25, runs the GQL on DuckGQL and the
Cypher on Neo4j, and reports where they disagree. Queries Neo4j cannot express are checked against
DuckGQL alone, with rewrites that must not change an answer (partitioning a filter, reversing paths,
`count(*)`, `DISTINCT`). Failing queries are reduced to minimal ones.

## Setup

- DuckGQL: `.cargo/config.toml` links the DuckGQL build's `libduckdb` (`DUCKDB_LIB_DIR`).
- Neo4j 2026.10, for instance:
  `docker run -d --name graphfuzz-neo4j -p 7474:7474 -p 7687:7687 -e NEO4J_AUTH=neo4j/graphfuzz123 neo4j:2026.10-community`

## Use

```sh
cargo build --release
./target/release/graphfuzz run --config config.yml          # writes report.md and results.jsonl
./target/release/graphfuzz generate --config config.yml --count 20 --dialect cypher --tags
./target/release/graphfuzz repro --config config.yml --gql "MATCH (n:Person) RETURN count(*) AS c"
./target/release/graphfuzz features                         # feature and group names for config.yml
```

`config.yml` documents every setting: seed, phases, per-feature and per-group weights, `focus`,
`disable`, and the checks.
