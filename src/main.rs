//! graphfuzz: generates graph queries from one AST, prints each in every engine's dialect (ISO GQL,
//! Cypher 25, openCypher, ...), runs them on every configured engine, and reports where engines disagree
//! with the reference engine or with themselves.

mod ast;
mod check;
mod config;
mod derive;
mod engine;
mod features;
mod g4;
mod generate;
mod grammar_run;
mod neo;
mod profile;
mod reduce;
mod render;
mod report;
mod rng;
mod schema;
#[cfg(test)]
mod tests;
mod value;
mod worker;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};

use check::{CaseResult, Checker, Reduced, Slot, Verdict, same_failure, signature};
use config::{FileConfig, Weights};
use engine::Outcome;
use generate::{Config, FEATURES, Gen, Generated, IMPLEMENTATION_DEFINED};
use profile::Profile;
use schema::Schema;
use value::multiset;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print generated queries in one dialect, one per line.
    Generate {
        /// The configuration file; its seed and generator settings apply.
        #[arg(long)]
        config: Option<PathBuf>,
        /// A built-in dialect profile (gql, cypher25, opencypher) or a profile file.
        #[arg(long, default_value = "gql")]
        dialect: String,
        #[arg(long, default_value_t = 20)]
        count: usize,
        /// Print the features each query uses after it.
        #[arg(long)]
        tags: bool,
    },
    /// List the features and feature groups the configuration can name.
    Features,
    /// Run generated queries on the configured engines and write a report.
    Run {
        #[arg(long, default_value = "config.yml")]
        config: PathBuf,
    },
    /// Run one query on the configured engines over the seed's fixture and print every answer.
    Repro {
        #[arg(long, default_value = "config.yml")]
        config: PathBuf,
        /// The query for GQL engines.
        #[arg(long)]
        gql: String,
        /// The query for Cypher engines; the GQL text when omitted, which Cypher 25 mostly accepts.
        #[arg(long)]
        cypher: Option<String>,
        /// Only these engines, comma-separated.
        #[arg(long)]
        engines: Option<String>,
    },
    /// Run only the configured grammars' systematic sentences.
    Grammar {
        #[arg(long, default_value = "config.yml")]
        config: PathBuf,
    },
    /// Show a configured grammar's coverage targets and the sentences that make them, without running them.
    Sentences {
        #[arg(long, default_value = "config.yml")]
        config: PathBuf,
        #[arg(long)]
        grammar: String,
        #[arg(long, default_value_t = 20)]
        count: usize,
        /// Instead, list this rule's alternatives and elements, as `present` and `types` number them.
        #[arg(long)]
        rule: Option<String>,
    },
    #[command(hide = true)]
    Worker {
        backend: String,
        #[arg(long)]
        path: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Worker { backend, path } => worker::worker_main(&backend, path),
        Command::Features => {
            for (feature, description) in FEATURES {
                println!("{feature:24} {description}");
            }
            println!("\nGroups:");
            for (group, members) in generate::GROUPS {
                println!("{group:24} {}", members.join(", "));
            }
            Ok(())
        }
        Command::Generate { config, dialect, count, tags } => {
            let file = FileConfig::load(config.as_deref())?;
            let base = config.as_deref().and_then(Path::parent).unwrap_or(Path::new(".")).to_path_buf();
            let profile = Profile::load(&dialect, &base, &[], &[], None)?;
            let gql = profile.name == "gql";
            let mut exclude: BTreeSet<&'static str> = features_named(&profile.unsupported);
            if !gql {
                exclude.extend(IMPLEMENTATION_DEFINED);
            }
            let schema = file.schema.build(gql)?;
            let config = Config { exclude, undirected: gql, isolate: None, weights: file.weights()? };
            let mut generator = Gen::new(file.seed, &schema, &config);
            for _ in 0..count {
                let generated = generator.generate();
                println!("{}", render::render(&generated.query, &profile));
                if tags {
                    println!("-- features: {}", generated.tags.iter().copied().collect::<Vec<_>>().join(", "));
                }
            }
            Ok(())
        }
        Command::Run { config } => run(&config),
        Command::Grammar { config } => {
            let file = FileConfig::load(Some(&config))?;
            fs::create_dir_all(&file.out)?;
            let schema = file.schema.build(false)?;
            let mut slots = connect_all(&file, &config, &schema)?;
            grammar_run::run_grammars(&file, &mut slots, &schema, &file.out)
        }
        Command::Sentences { config, grammar, count, rule } => {
            let file = FileConfig::load(Some(&config))?;
            let Some(settings) = file.grammars.get(&grammar) else { bail!("no grammar named {grammar} in the configuration") };
            let parsed = g4::Grammar::load(&settings.file)?;
            let start = settings.start.clone().unwrap_or_else(|| parsed.order[0].clone());
            let deriver = derive::Deriver::new(&parsed, &start, &settings.separator, &settings.vocabulary, &settings.prefer, &settings.typing);
            if let Some(rule) = &rule {
                print!("{}", deriver.describe_rule(rule));
                return Ok(());
            }
            let targets = deriver.targets(settings.coverage);
            let parser_rules = parsed.rules.values().filter(|rule| !rule.lexer).count();
            let reachable = parsed.rules.keys().filter(|name| !parsed.rules[*name].lexer && deriver.reachable(name)).count();
            println!("{}: {} parser rules (with lifted groups), {} reachable from {}, {} targets", parsed.name, parser_rules, reachable, start, targets.len());
            let mut seen = BTreeSet::new();
            let mut covered = BTreeSet::new();
            let mut sentences = 0;
            for target in &targets {
                if covered.contains(target) {
                    continue;
                }
                let sentence = deriver.sentence(target);
                covered.extend(sentence.covered.iter().cloned());
                if seen.insert(sentence.text.clone()) {
                    if sentences < count {
                        println!("{:60} {}", target.describe(&parsed), sentence.text);
                    }
                    sentences += 1;
                }
            }
            println!(
                "{} distinct sentences cover {} of {} targets",
                sentences,
                targets.iter().filter(|target| covered.contains(*target)).count(),
                targets.len()
            );
            Ok(())
        }
        Command::Repro { config, gql, cypher, engines } => {
            let file = FileConfig::load(Some(&config))?;
            let only: Option<BTreeSet<String>> = engines.map(|names| names.split(',').map(|name| name.trim().to_string()).collect());
            let schema = file.schema.build(false)?;
            let mut slots = connect_all(&file, &config, &schema)?;
            for slot in &mut slots {
                if only.as_ref().is_some_and(|only| !only.contains(&slot.name)) {
                    continue;
                }
                let text = if slot.profile.name == "gql" { gql.clone() } else { cypher.clone().unwrap_or_else(|| gql.clone()) };
                match slot.engine.read(&format!("{}{}", slot.profile.prefix, text))? {
                    Outcome::Rows { columns, rows } => {
                        println!("{}: {} rows ({})", slot.name, rows.len(), columns.join(", "));
                        for key in multiset(&rows) {
                            println!("  {key}");
                        }
                    }
                    other => println!("{}: {other:?}", slot.name),
                }
            }
            Ok(())
        }
    }
}

fn features_named(names: &[String]) -> BTreeSet<&'static str> {
    FEATURES.iter().map(|(feature, _)| *feature).filter(|feature| names.iter().any(|name| name == feature)).collect()
}

/// Connects every enabled engine, loads the fixture into each, and checks that they all hold the same graph.
fn connect_all(file: &FileConfig, config_path: &Path, schema: &Schema) -> Result<Vec<Slot>> {
    let base = config_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let timeout = Duration::from_millis(file.timeout_ms);
    let undirected = file.schema.build(true)?;
    let mut slots = Vec::new();
    for (name, config) in &file.engines {
        if !config.enabled {
            continue;
        }
        if !engine::compiled_in(&config.connector) {
            if file.reference.as_deref() == Some(name.as_str()) {
                bail!("the reference engine '{name}' needs the {} feature; rebuild with --features {0}", config.connector);
            }
            eprintln!("skipping {name}: built without the {} feature (rebuild with --features {0})", config.connector);
            continue;
        }
        let profile = Profile::load(&config.dialect, &base, &config.supported, &config.unsupported, config.syntax.as_ref())?;
        let mut engine = engine::connect(name, config, &base, timeout)?;
        engine.setup(schema, &undirected, file.seed, &profile)?;
        slots.push(Slot { name: name.clone(), engine, profile, metamorphic: config.metamorphic, undirected_graph: config.connector == "duckgql" });
    }
    if slots.is_empty() {
        bail!("no engine is enabled");
    }
    let mut first: Option<(String, Vec<Vec<String>>)> = None;
    for slot in &mut slots {
        let mut digests = Vec::new();
        for digest in check::digest_queries(schema) {
            match slot.engine.read(&format!("{}{}", slot.profile.prefix, digest))? {
                Outcome::Rows { rows, .. } => digests.push(multiset(&rows)),
                other => bail!("{}: reading back the fixture failed: {other:?}\n{digest}", slot.name),
            }
        }
        match &first {
            None => first = Some((slot.name.clone(), digests)),
            Some((name, expected)) if *expected != digests => bail!("{} and {} hold different fixtures", name, slot.name),
            Some(_) => {}
        }
    }
    Ok(slots)
}

/// A phase of a run: which queries it generates, on which engines, against which reference.
struct Phase {
    name: String,
    config: Config,
    count: usize,
    targets: Vec<usize>,
    reference: Option<usize>,
}

fn run(config_path: &Path) -> Result<()> {
    let file = FileConfig::load(Some(config_path))?;
    fs::create_dir_all(&file.out)?;
    let started = Instant::now();
    let compared = file.schema.build(false)?;
    let undirected = file.schema.build(true)?;
    let weights = file.weights()?;
    let mut slots = connect_all(&file, config_path, &compared)?;
    let names: Vec<String> = slots.iter().map(|slot| slot.name.clone()).collect();
    let reference = file.reference.as_ref().and_then(|name| names.iter().position(|candidate| candidate == name));
    let targets: Vec<usize> = (0..slots.len()).filter(|index| Some(*index) != reference).collect();
    let undirected_targets: Vec<usize> = targets.iter().copied().filter(|index| slots[*index].undirected_graph).collect();
    let mut reference_unsupported: BTreeSet<&'static str> = reference.map(|index| features_named(&slots[index].profile.unsupported)).unwrap_or_default();
    reference_unsupported.extend(IMPLEMENTATION_DEFINED);

    let mut phases = Vec::new();
    for feature in file.isolated_features()? {
        let undirected_feature = feature.contains("undirected");
        phases.push(Phase {
            name: format!("isolate:{feature}"),
            config: Config { exclude: BTreeSet::new(), undirected: undirected_feature, isolate: Some(feature.to_string()), weights: Weights::default() },
            count: file.phases.isolate,
            targets: if undirected_feature { undirected_targets.clone() } else { targets.clone() },
            reference: if undirected_feature { None } else { reference },
        });
    }
    phases.push(Phase {
        name: "compare".to_string(),
        config: Config { exclude: reference_unsupported.clone(), undirected: false, isolate: None, weights: weights.clone() },
        count: file.phases.compare,
        targets: targets.clone(),
        reference,
    });
    phases.push(Phase {
        name: "explore".to_string(),
        config: Config { exclude: BTreeSet::new(), undirected: true, isolate: None, weights: weights.clone() },
        count: if undirected_targets.is_empty() { 0 } else { file.phases.explore },
        targets: undirected_targets.clone(),
        reference: None,
    });

    let mut results: Vec<CaseResult> = Vec::new();
    // Each case's query and its phase, for reduction afterwards.
    let mut cases: Vec<(Generated, usize)> = Vec::new();
    let total: usize = phases.iter().map(|phase| phase.count).sum();
    for (phase_index, phase) in phases.iter().enumerate() {
        if phase.count == 0 || phase.targets.is_empty() {
            continue;
        }
        let schema = if phase.config.undirected { &undirected } else { &compared };
        for slot in slots.iter_mut() {
            slot.engine.use_undirected(phase.config.undirected);
        }
        let mut generator = Gen::new(file.seed.wrapping_mul(1_000_003).wrapping_add(phase_index as u64), schema, &phase.config);
        let mut checker = Checker { slots: &mut slots, reference: phase.reference, schema };
        for _ in 0..phase.count {
            let generated = generator.generate();
            let result = checker.check(results.len(), &phase.name, &generated, &phase.targets)?;
            results.push(result);
            cases.push((generated, phase_index));
            if results.len().is_multiple_of(100) {
                let wrong = results
                    .iter()
                    .flat_map(|case| case.verdicts.values())
                    .filter(|verdict| matches!(verdict, Verdict::Mismatch { .. } | Verdict::Metamorphic { .. }))
                    .count();
                eprintln!("{}/{} cases, {} wrong answers, {:.0}s", results.len(), total, wrong, started.elapsed().as_secs_f64());
            }
        }
    }
    if file.checks.reduce {
        reduce_failures(&mut results, &cases, &phases, &mut slots, &compared, &undirected, file.checks.reduce_budget)?;
    }
    let mut jsonl = fs::File::create(file.out.join("results.jsonl"))?;
    for result in &results {
        serde_json::to_writer(&mut jsonl, result)?;
        jsonl.write_all(b"\n")?;
    }
    let restarts: Vec<String> =
        slots.iter().filter(|slot| slot.engine.restarts() > 0).map(|slot| format!("{} {}", slot.name, slot.engine.restarts())).collect();
    let header = format!(
        "Seed {}; {} cases in {:.0}s; reference {}; worker restarts: {}.\n\nConfiguration:\n\n```yaml\n{}```",
        file.seed,
        results.len(),
        started.elapsed().as_secs_f64(),
        file.reference.as_deref().unwrap_or("none"),
        if restarts.is_empty() { "none".to_string() } else { restarts.join(", ") },
        file.raw
    );
    let target_names: Vec<String> = targets.iter().map(|index| names[*index].clone()).collect();
    let reference_profile = reference.map(|index| (names[index].as_str(), &slots[index].profile));
    let markdown = report::markdown(&results, &header, &target_names, reference_profile);
    fs::write(file.out.join("report.md"), &markdown)?;
    eprintln!("wrote {} and {}", file.out.join("report.md").display(), file.out.join("results.jsonl").display());
    if file.phases.grammar && !file.grammars.is_empty() {
        for slot in slots.iter_mut() {
            slot.engine.use_undirected(false);
        }
        grammar_run::run_grammars(&file, &mut slots, &compared, &file.out)?;
    }
    Ok(())
}

/// The key a failure is grouped under for one engine; one case of each group is reduced.
fn failure_group(result: &CaseResult, verdict: &Verdict) -> Option<String> {
    let tags = || result.tags.iter().copied().collect::<Vec<_>>().join(",");
    Some(match verdict {
        // A feature run alone gets its own group, so a catch-all message still yields one example per feature.
        Verdict::TargetRejects { message, .. } | Verdict::TargetOnlyError { message, .. } if result.phase.starts_with("isolate:") => {
            format!("error:{}:{}", result.phase, signature(message))
        }
        Verdict::TargetRejects { message, .. } | Verdict::TargetOnlyError { message, .. } => format!("error:{}", signature(message)),
        Verdict::Metamorphic { oracle, .. } => format!("oracle:{oracle}:{}", tags()),
        Verdict::Mismatch { .. } => format!("mismatch:{}", tags()),
        Verdict::MatchMode { .. } => "match_mode".to_string(),
        Verdict::Crash(_) => "crash".to_string(),
        Verdict::Timeout => "timeout".to_string(),
        _ => return None,
    })
}

/// Shrinks the shortest case of each engine's failure groups to a query that still fails the same way.
fn reduce_failures(
    results: &mut [CaseResult],
    cases: &[(Generated, usize)],
    phases: &[Phase],
    slots: &mut [Slot],
    compared: &Schema,
    undirected: &Schema,
    budget: usize,
) -> Result<()> {
    let mut representatives: BTreeMap<(String, String), usize> = BTreeMap::new();
    for (index, result) in results.iter().enumerate() {
        for (engine, verdict) in &result.verdicts {
            if let Some(group) = failure_group(result, verdict) {
                let best = representatives.entry((engine.clone(), group)).or_insert(index);
                if results[*best].gql.len() > result.gql.len() {
                    *best = index;
                }
            }
        }
    }
    let started = Instant::now();
    eprintln!("reducing {} failure groups", representatives.len());
    for ((engine, _), index) in representatives {
        let (generated, phase_index) = &cases[index];
        let phase = &phases[*phase_index];
        let Some(target) = slots.iter().position(|slot| slot.name == engine) else { continue };
        let schema = if phase.config.undirected { undirected } else { compared };
        for slot in slots.iter_mut() {
            slot.engine.use_undirected(phase.config.undirected);
        }
        let original = results[index].verdicts[&engine].clone();
        let mut checker = Checker { slots: &mut *slots, reference: phase.reference, schema };
        let reduced = reduce::reduce(&generated.query, budget, |candidate| {
            let candidate =
                Generated { query: candidate.clone(), tags: features::of(candidate), mutation: generated.mutation, partition: generated.partition.clone() };
            match checker.check_one(target, &candidate) {
                Ok(verdict) => same_failure(&original, &verdict),
                Err(_) => false,
            }
        });
        // In the engine's own dialect, so the reduced query can be run on it as it stands.
        let query = render::render(&reduced, &slots[target].profile);
        results[index].reduced.insert(engine, Reduced { query, features: features::of(&reduced).into_iter().collect() });
    }
    eprintln!("reduced in {:.0}s", started.elapsed().as_secs_f64());
    Ok(())
}
