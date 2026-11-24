//! graphfuzz: generates graph queries from one AST as ISO GQL and as Cypher, runs the GQL on DuckGQL and
//! the Cypher on Neo4j, and reports wrong answers, crashes, and unsupported features.

mod ast;
mod check;
mod config;
mod duck;
mod features;
mod generate;
mod neo;
mod reduce;
mod render;
mod report;
mod rng;
mod schema;
mod value;

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use clap::{Parser, Subcommand, ValueEnum};

use config::{FileConfig, Weights};
use check::{CaseResult, Checker, Verdict, same_failure, signature};
use duck::{Duck, Statement, StatementResult};
use generate::{Config, FEATURES, GQL_ONLY, Gen, Generated};
use neo::Neo;
use schema::Schema;
use value::multiset;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum DialectArg {
    Gql,
    Cypher,
}

#[derive(Subcommand)]
enum Command {
    /// Print generated queries, one per line.
    Generate {
        /// The configuration file; its seed and generator settings apply.
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long, value_enum, default_value = "gql")]
        dialect: DialectArg,
        #[arg(long, default_value_t = 20)]
        count: usize,
        /// Also generate GQL-only constructs (no Cypher is printed for those).
        #[arg(long)]
        gql_only: bool,
        /// Print the features each query uses after it.
        #[arg(long)]
        tags: bool,
    },
    /// List the features and feature groups the configuration can name.
    Features,
    /// Run generated queries on DuckGQL and Neo4j and write a report, as the configuration file says.
    Run {
        #[arg(long, default_value = "config.yml")]
        config: PathBuf,
    },
    /// Run one query on both engines over the configured seed's fixture and print both answers.
    Repro {
        #[arg(long, default_value = "config.yml")]
        config: PathBuf,
        /// The query for DuckGQL.
        #[arg(long)]
        gql: String,
        /// The query for Neo4j; the GQL text when omitted, which Cypher 25 mostly accepts.
        #[arg(long)]
        cypher: Option<String>,
        /// Use the graph with an undirected edge type, which Neo4j does not hold.
        #[arg(long)]
        undirected: bool,
    },
    #[command(hide = true)]
    Worker,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Worker => duck::worker_main(),
        Command::Features => {
            for (feature, description) in FEATURES {
                let neo = if GQL_ONLY.contains(feature) { "  (GQL only)" } else { "" };
                println!("{feature:24} {description}{neo}");
            }
            println!("\nGroups:");
            for (group, members) in generate::GROUPS {
                println!("{group:24} {}", members.join(", "));
            }
            Ok(())
        }
        Command::Generate { config, dialect, count, gql_only, tags } => {
            let file = FileConfig::load(config.as_deref())?;
            let schema = Schema::social(gql_only);
            let config = Config { comparable: !gql_only, undirected: gql_only, isolate: None, weights: file.weights()? };
            let mut generator = Gen::new(file.seed, &schema, &config);
            for _ in 0..count {
                let generated = generator.generate();
                let text = match dialect {
                    DialectArg::Gql => Some(render::gql(&generated.query)),
                    DialectArg::Cypher => render::cypher(&generated.query),
                };
                match text {
                    Some(text) => println!("{text}"),
                    None => println!("-- GQL only: {}", render::gql(&generated.query)),
                }
                if tags {
                    println!("-- features: {}", generated.tags.iter().copied().collect::<Vec<_>>().join(", "));
                }
            }
            Ok(())
        }
        Command::Run { config } => run(FileConfig::load(Some(&config))?),
        Command::Repro { config, gql, cypher, undirected } => {
            let file = FileConfig::load(Some(&config))?;
            let schema = Schema::social(false);
            let mut duck = Duck::new(duck_setup(file.seed), Duration::from_millis(file.timeout_ms));
            let graph = if undirected { "fuzzu" } else { "fuzz" };
            let statements = vec![Statement { sql: format!("SESSION SET GRAPH {graph}"), read: false }, Statement { sql: gql.clone(), read: true }];
            let print = |engine: &str, result: &StatementResult| match result {
                StatementResult::Rows { columns, rows } => {
                    println!("{engine}: {} rows ({})", rows.len(), columns.join(", "));
                    for key in multiset(rows) {
                        println!("  {key}");
                    }
                }
                other => println!("{engine}: {other:?}"),
            };
            match duck.run(&statements)? {
                duck::RunOutcome::Results(results) => print("DuckGQL", &results[1]),
                other => println!("DuckGQL: {other:?}"),
            }
            if file.neo4j.enabled && !undirected {
                let neo = Neo::new(&file.neo4j.url, &file.neo4j.user, &file.neo4j.password, Duration::from_millis(file.timeout_ms * 3));
                load_neo4j(&neo, &mut duck, &schema, file.seed)?;
                let text = cypher.unwrap_or_else(|| format!("CYPHER 25 {gql}"));
                print("Neo4j", &neo.run(&text)?);
            }
            Ok(())
        }
    }
}

/// DuckGQL gets two graphs: `fuzz`, which Neo4j also holds, and `fuzzu`, which adds an undirected edge type.
fn duck_setup(seed: u64) -> Vec<Statement> {
    let mut statements = Vec::new();
    for (name, undirected) in [("fuzz", false), ("fuzzu", true)] {
        let schema = Schema::social(undirected);
        statements.push(schema.gql_create_graph(name));
        statements.push(format!("SESSION SET GRAPH {name}"));
        statements.extend(schema.fixture(seed).gql_statements());
    }
    statements.into_iter().map(|sql| Statement { sql, read: false }).collect()
}

/// Loads the fixture into Neo4j and checks that both engines now hold the same graph.
fn load_neo4j(neo: &Neo, duck: &mut Duck, schema: &Schema, seed: u64) -> Result<()> {
    for statement in schema.fixture(seed).cypher_statements() {
        if let StatementResult::Error(error) = neo.run(&statement)? {
            bail!("loading the Neo4j fixture failed: {error}\n{statement}");
        }
    }
    for digest in check::digest_queries(schema) {
        let statements = vec![Statement { sql: "SESSION SET GRAPH fuzz".to_string(), read: false }, Statement { sql: digest.clone(), read: true }];
        let duck_rows = match duck.run(&statements)? {
            duck::RunOutcome::Results(results) => match &results[1] {
                StatementResult::Rows { rows, .. } => rows.clone(),
                other => bail!("DuckGQL fixture digest failed: {other:?}"),
            },
            other => bail!("DuckGQL fixture digest failed: {other:?}"),
        };
        let neo_rows = match neo.run(&format!("CYPHER 25 {digest}"))? {
            StatementResult::Rows { rows, .. } => rows,
            other => bail!("Neo4j fixture digest failed: {other:?}"),
        };
        if multiset(&duck_rows) != multiset(&neo_rows) {
            bail!("the engines hold different fixtures for: {digest}");
        }
    }
    Ok(())
}

fn run(file: FileConfig) -> Result<()> {
    fs::create_dir_all(&file.out)?;
    let weights = file.weights()?;
    let timeout = Duration::from_millis(file.timeout_ms);
    let started = Instant::now();
    let compared = Schema::social(false);
    let undirected = Schema::social(true);
    let mut duck = Duck::new(duck_setup(file.seed), timeout);
    let neo = if file.neo4j.enabled {
        let neo = Neo::new(&file.neo4j.url, &file.neo4j.user, &file.neo4j.password, timeout * 3);
        load_neo4j(&neo, &mut duck, &compared, file.seed)?;
        Some(neo)
    } else {
        None
    };

    // Each phase: its generator configuration and number of queries.
    struct Phase {
        name: String,
        config: Config,
        count: usize,
    }
    let mut phases = Vec::new();
    for feature in file.isolated_features()? {
        let gql_only = GQL_ONLY.contains(&feature);
        phases.push(Phase {
            name: format!("isolate:{feature}"),
            config: Config {
                comparable: !gql_only,
                undirected: feature.contains("undirected"),
                isolate: Some(feature.to_string()),
                weights: Weights::default(),
            },
            count: file.phases.isolate,
        });
    }
    phases.push(Phase {
        name: "compare".to_string(),
        config: Config { comparable: true, undirected: false, isolate: None, weights: weights.clone() },
        count: file.phases.compare,
    });
    phases.push(Phase {
        name: "explore".to_string(),
        config: Config { comparable: false, undirected: true, isolate: None, weights: weights.clone() },
        count: file.phases.explore,
    });

    let mut results: Vec<CaseResult> = Vec::new();
    // Each case's query and where it ran, for reduction afterwards.
    let mut cases: Vec<(Generated, bool, bool)> = Vec::new();
    let total: usize = phases.iter().map(|phase| phase.count).sum();
    for (phase_index, phase) in phases.iter().enumerate() {
        let (schema, graph) = if phase.config.undirected { (&undirected, "fuzzu") } else { (&compared, "fuzz") };
        let mut generator = Gen::new(file.seed.wrapping_mul(1_000_003).wrapping_add(phase_index as u64), schema, &phase.config);
        let use_neo = phase.config.comparable && !phase.config.undirected;
        let mut checker = Checker { duck: &mut duck, neo: if use_neo { neo.as_ref() } else { None }, schema, graph, metamorphic: file.checks.metamorphic };
        for _ in 0..phase.count {
            let generated = generator.generate();
            let result = checker.check(results.len(), &phase.name, &generated)?;
            results.push(result);
            cases.push((generated, phase.config.undirected, use_neo));
            if results.len() % 100 == 0 {
                let wrong = results.iter().filter(|case| matches!(case.verdict, Verdict::Mismatch { .. } | Verdict::Metamorphic { .. })).count();
                eprintln!("{}/{} cases, {} wrong answers, {:.0}s", results.len(), total, wrong, started.elapsed().as_secs_f64());
            }
        }
    }
    if file.checks.reduce {
        reduce_failures(&mut results, &cases, &mut duck, neo.as_ref(), &compared, &undirected, file.checks.metamorphic, file.checks.reduce_budget)?;
    }
    let mut jsonl = fs::File::create(file.out.join("results.jsonl"))?;
    for result in &results {
        serde_json::to_writer(&mut jsonl, result)?;
        jsonl.write_all(b"\n")?;
    }
    let header = format!(
        "Seed {}; {} cases in {:.0}s; Neo4j {}; {} DuckGQL worker restarts.\n\nConfiguration:\n\n```yaml\n{}```",
        file.seed,
        results.len(),
        started.elapsed().as_secs_f64(),
        if neo.is_some() { "compared" } else { "not used" },
        duck.restarts,
        file.raw
    );
    let markdown = report::markdown(&results, &header);
    fs::write(file.out.join("report.md"), &markdown)?;
    eprintln!("wrote {} and {}", file.out.join("report.md").display(), file.out.join("results.jsonl").display());
    Ok(())
}

/// The key a failure is grouped under in the report; one case of each group is reduced.
fn failure_group(result: &CaseResult) -> Option<String> {
    Some(match &result.verdict {
        // A feature run alone gets its own group, so a catch-all message still yields one example per feature.
        Verdict::DuckRejects { message, .. } | Verdict::DuckOnlyError { message, .. } if result.phase.starts_with("isolate:") => {
            format!("error:{}:{}", result.phase, signature(message))
        }
        Verdict::DuckRejects { message, .. } | Verdict::DuckOnlyError { message, .. } => format!("error:{}", signature(message)),
        Verdict::Metamorphic { oracle, .. } => format!("oracle:{oracle}:{}", result.tags.iter().copied().collect::<Vec<_>>().join(",")),
        Verdict::Mismatch { .. } => format!("mismatch:{}", result.tags.iter().copied().collect::<Vec<_>>().join(",")),
        Verdict::Crash(_) => "crash".to_string(),
        Verdict::Timeout => "timeout".to_string(),
        _ => return None,
    })
}

/// Shrinks the shortest case of each failure group to a query that still fails the same way.
fn reduce_failures(
    results: &mut [CaseResult],
    cases: &[(Generated, bool, bool)],
    duck: &mut Duck,
    neo: Option<&Neo>,
    compared: &Schema,
    undirected: &Schema,
    metamorphic: bool,
    budget: usize,
) -> Result<()> {
    let mut representatives: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for (index, result) in results.iter().enumerate() {
        if let Some(group) = failure_group(result) {
            let best = representatives.entry(group).or_insert(index);
            if results[*best].gql.len() > result.gql.len() {
                *best = index;
            }
        }
    }
    let started = Instant::now();
    eprintln!("reducing {} failure groups", representatives.len());
    for (_, index) in representatives {
        let (generated, is_undirected, use_neo) = &cases[index];
        let (schema, graph) = if *is_undirected { (undirected, "fuzzu") } else { (compared, "fuzz") };
        let mut checker = Checker { duck: &mut *duck, neo: if *use_neo { neo } else { None }, schema, graph, metamorphic };
        let original = results[index].verdict.clone();
        let reduced = reduce::reduce(&generated.query, budget, |candidate| {
            let candidate = Generated { query: candidate.clone(), tags: features::of(candidate), mutation: generated.mutation, partition: generated.partition.clone() };
            match checker.check(index, "reduce", &candidate) {
                Ok(result) => same_failure(&original, &result.verdict),
                Err(_) => false,
            }
        });
        results[index].reduced = Some(render::gql(&reduced));
        results[index].reduced_features = features::of(&reduced).into_iter().collect();
    }
    eprintln!("reduced in {:.0}s", started.elapsed().as_secs_f64());
    Ok(())
}
