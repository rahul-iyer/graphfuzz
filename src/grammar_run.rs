//! Runs every configured grammar's systematic sentences on that grammar's engines, attributes failures to the
//! grammar choice that causes them, and writes `grammar.md` and `grammar.jsonl`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Write;
use std::path::Path;
use std::time::Instant;

use anyhow::Result;
use serde::Serialize;

use crate::check::{ErrorKind, Slot, classify, digest_queries, signature};
use crate::config::{FileConfig, GrammarConfig};
use crate::derive::{Deriver, Target};
use crate::engine::Outcome;
use crate::g4::Grammar;
use crate::schema::Schema;
use crate::value::multiset;

/// How one engine answered one sentence.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub enum Answer {
    Rows(usize),
    Done,
    Error { kind: ErrorKind, signature: String },
    Crash,
    Timeout,
    Skipped(String),
}

impl Answer {
    fn failed(&self) -> bool {
        matches!(self, Answer::Error { .. } | Answer::Crash | Answer::Timeout)
    }

    fn label(&self) -> String {
        match self {
            Answer::Rows(_) | Answer::Done => "ok".to_string(),
            Answer::Error { kind, .. } => format!("{kind:?}"),
            Answer::Crash => "Crash".to_string(),
            Answer::Timeout => "Timeout".to_string(),
            Answer::Skipped(_) => "Skipped".to_string(),
        }
    }

    fn message(&self) -> String {
        match self {
            Answer::Error { signature, .. } => signature.clone(),
            Answer::Crash => "crash".to_string(),
            Answer::Timeout => "timeout".to_string(),
            other => other.label(),
        }
    }
}

#[derive(Serialize)]
struct TargetResult {
    grammar: String,
    target: String,
    rule: String,
    sentence: String,
    baseline: String,
    answers: BTreeMap<String, Answer>,
    /// Engines that fail this sentence differently from its baseline: the target's choice is the cause.
    own_failures: Vec<String>,
}

struct Disagreement {
    sentence: String,
    target: String,
    answers: Vec<(String, Vec<String>)>,
}

/// The answer and, for rows, their multiset, so engines that both answer can be compared.
fn answer(outcome: Outcome) -> (Answer, Option<Vec<String>>) {
    match outcome {
        Outcome::Rows { rows, .. } => (Answer::Rows(rows.len()), Some(multiset(&rows))),
        Outcome::Done => (Answer::Done, None),
        Outcome::Error(message) => (Answer::Error { kind: classify(&message), signature: signature(&message) }, None),
        Outcome::Crash(_) => (Answer::Crash, None),
        Outcome::Timeout => (Answer::Timeout, None),
        Outcome::Transport(message) => (Answer::Skipped(message), None),
    }
}

fn digests(slot: &mut Slot, schema: &Schema) -> Result<Vec<Vec<String>>> {
    let mut result = Vec::new();
    for digest in digest_queries(schema) {
        match slot.engine.read(&format!("{}{}", slot.profile.prefix, digest))? {
            Outcome::Rows { rows, .. } => result.push(multiset(&rows)),
            other => result.push(vec![format!("{other:?}")]),
        }
    }
    Ok(result)
}

pub fn run_grammars(file: &FileConfig, slots: &mut [Slot], schema: &Schema, out: &Path) -> Result<()> {
    let mut report = String::new();
    let _ = writeln!(report, "# graphfuzz grammar report\n\nSeed {}. Every choice each grammar offers, each tried with the shortest sentence that makes it, in a rolled-back transaction.\n", file.seed);
    let mut jsonl = std::fs::File::create(out.join("grammar.jsonl"))?;
    let undirected = file.schema.build(true)?;
    for (name, settings) in &file.grammars {
        if !settings.enabled {
            continue;
        }
        run_grammar(name, settings, slots, schema, &undirected, file.seed, &mut report, &mut jsonl)?;
    }
    std::fs::write(out.join("grammar.md"), report)?;
    eprintln!("wrote {} and {}", out.join("grammar.md").display(), out.join("grammar.jsonl").display());
    Ok(())
}

fn run_grammar(
    name: &str,
    settings: &GrammarConfig,
    slots: &mut [Slot],
    schema: &Schema,
    undirected: &Schema,
    seed: u64,
    report: &mut String,
    jsonl: &mut std::fs::File,
) -> Result<()> {
    let started = Instant::now();
    let grammar = Grammar::load(&settings.file)?;
    let start = settings.start.clone().unwrap_or_else(|| grammar.order[0].clone());
    let deriver = Deriver::new(&grammar, &start, &settings.separator, &settings.vocabulary, &settings.prefer);
    let mut targets = deriver.targets(settings.coverage);
    if settings.limit > 0 {
        targets.truncate(settings.limit);
    }
    let engines: Vec<usize> = settings.engines.iter().filter_map(|engine| slots.iter().position(|slot| &slot.name == engine)).collect();

    // One sentence per target, and the baseline each is judged against.
    let mut sentences: Vec<(Target, String, String)> = Vec::new();
    let mut distinct: BTreeSet<String> = BTreeSet::new();
    for target in &targets {
        let sentence = deriver.sentence(target).text;
        let baseline = deriver.sentence(&target.baseline(deriver.default_alt(target.rule()))).text;
        distinct.insert(sentence.clone());
        distinct.insert(baseline.clone());
        sentences.push((target.clone(), sentence, baseline));
    }
    eprintln!("grammar {name}: {} targets, {} distinct sentences, on {}", targets.len(), distinct.len(), settings.engines.join(", "));

    let initial: Vec<Vec<Vec<String>>> = engines.iter().map(|index| digests(&mut slots[*index], schema)).collect::<Result<_>>()?;
    let mut answers: BTreeMap<String, BTreeMap<String, (Answer, Option<Vec<String>>)>> = BTreeMap::new();
    let mut reloads: BTreeMap<String, usize> = BTreeMap::new();
    for (count, sentence) in distinct.iter().enumerate() {
        let mut per_engine = BTreeMap::new();
        for &index in &engines {
            let slot = &mut slots[index];
            let outcome = slot.engine.sandboxed(&format!("{}{}", settings.prefix, sentence))?;
            per_engine.insert(slot.name.clone(), answer(outcome));
        }
        answers.insert(sentence.clone(), per_engine);
        // A sentence the transaction did not contain (a commit, a session change) must not leak into the next.
        if (count + 1) % 250 == 0 || count + 1 == distinct.len() {
            for (position, &index) in engines.iter().enumerate() {
                if digests(&mut slots[index], schema)? != initial[position] {
                    let profile = slots[index].profile.clone();
                    slots[index].engine.setup(schema, undirected, seed, &profile)?;
                    *reloads.entry(slots[index].name.clone()).or_default() += 1;
                }
            }
            eprintln!("grammar {name}: {}/{} sentences, {:.0}s", count + 1, distinct.len(), started.elapsed().as_secs_f64());
        }
    }

    let names: Vec<String> = engines.iter().map(|index| slots[*index].name.clone()).collect();
    let mut results = Vec::new();
    let mut disagreements = Vec::new();
    let mut compared_sentences = BTreeSet::new();
    for (target, sentence, baseline) in &sentences {
        let own: Vec<String> = names
            .iter()
            .filter(|engine| {
                let here = &answers[sentence][*engine].0;
                let before = &answers[baseline][*engine].0;
                here.failed() && (!before.failed() || before.message() != here.message())
            })
            .cloned()
            .collect();
        // Engines that all answered with rows must agree.
        if compared_sentences.insert(sentence.clone()) {
            let rows: Vec<(String, Vec<String>)> =
                names.iter().filter_map(|engine| answers[sentence][engine].1.clone().map(|rows| (engine.clone(), rows))).collect();
            if rows.len() >= 2 && rows.iter().any(|(_, keys)| *keys != rows[0].1) {
                disagreements.push(Disagreement { sentence: sentence.clone(), target: target.describe(&grammar), answers: rows });
            }
        }
        results.push(TargetResult {
            grammar: name.to_string(),
            target: target.describe(&grammar),
            rule: grammar.owner(target.rule()).to_string(),
            sentence: sentence.clone(),
            baseline: baseline.clone(),
            answers: names.iter().map(|engine| (engine.clone(), answers[sentence][engine].0.clone())).collect(),
            own_failures: own,
        });
    }
    for result in &results {
        serde_json::to_writer(&mut *jsonl, result)?;
        jsonl.write_all(b"\n")?;
    }

    // The report: totals, what fails on its own by rule and by message, crashes, and disagreements.
    let _ = writeln!(
        report,
        "## {name}: {} ({} parser rules with lifted groups)\n\n{} targets ({:?} coverage), {} distinct sentences, {:.0}s. Fixture reloads after drift: {}.\n",
        settings.file.display(),
        grammar.rules.values().filter(|rule| !rule.lexer).count(),
        targets.len(),
        settings.coverage,
        distinct.len(),
        started.elapsed().as_secs_f64(),
        if reloads.is_empty() { "none".to_string() } else { reloads.iter().map(|(engine, count)| format!("{engine} {count}")).collect::<Vec<_>>().join(", ") }
    );
    let _ = writeln!(report, "| engine | sentences | ok | NotImplemented | Parser | Binder | Runtime | Internal | Crash | Timeout | targets failing on their own |\n|---|---|---|---|---|---|---|---|---|---|---|");
    for engine in &names {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for per_engine in answers.values() {
            *counts.entry(per_engine[engine].0.label()).or_default() += 1;
        }
        let own = results.iter().filter(|result| result.own_failures.contains(engine)).count();
        let get = |key: &str| counts.get(key).copied().unwrap_or(0);
        let _ = writeln!(
            report,
            "| {engine} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {own} |",
            answers.len(),
            get("ok"),
            get("NotImplemented"),
            get("Parser"),
            get("Binder"),
            get("Runtime"),
            get("Internal"),
            get("Crash"),
            get("Timeout")
        );
    }
    for engine in &names {
        let own: Vec<&TargetResult> = results.iter().filter(|result| result.own_failures.contains(engine)).collect();
        // Crashes and internal errors first: they are bugs whatever the sentence means.
        let severe: Vec<&&TargetResult> = own.iter().filter(|result| matches!(result.answers[engine], Answer::Crash | Answer::Timeout | Answer::Error { kind: ErrorKind::Internal, .. })).collect();
        if !severe.is_empty() {
            let _ = writeln!(report, "\n### {name} on {engine}: crashes, timeouts, and internal errors ({})\n", severe.len());
            let mut seen = BTreeSet::new();
            for result in severe {
                if seen.insert(result.answers[engine].message()) {
                    let _ = writeln!(report, "- `{}` — {}\n  ```\n  {}\n  ```", result.answers[engine].message().replace('`', "'"), result.target, result.sentence);
                }
            }
        }
        let mut by_rule: BTreeMap<&str, Vec<&TargetResult>> = BTreeMap::new();
        for result in &own {
            by_rule.entry(result.rule.as_str()).or_default().push(result);
        }
        let mut rules: Vec<(&str, Vec<&TargetResult>)> = by_rule.into_iter().collect();
        rules.sort_by_key(|(_, group)| std::cmp::Reverse(group.len()));
        let _ = writeln!(report, "\n### {name} on {engine}: rules whose choices fail on their own ({} rules)\n", rules.len());
        let _ = writeln!(report, "| rule | failing choices | most common error | shortest failing sentence |\n|---|---|---|---|");
        for (rule, group) in &rules {
            let mut messages: BTreeMap<String, usize> = BTreeMap::new();
            for result in group {
                *messages.entry(result.answers[engine].message()).or_default() += 1;
            }
            let common = messages.iter().max_by_key(|(_, count)| **count).map(|(message, _)| message.clone()).unwrap_or_default();
            let shortest = group.iter().min_by_key(|result| result.sentence.len()).unwrap();
            let _ = writeln!(report, "| {rule} | {} | `{}` | `{}` |", group.len(), common.replace('|', "\\|").replace('`', "'"), shortest.sentence.replace('|', "\\|").replace('`', "'"));
        }
        let mut by_message: BTreeMap<String, Vec<&TargetResult>> = BTreeMap::new();
        for result in &own {
            by_message.entry(result.answers[engine].message()).or_default().push(result);
        }
        let mut messages: Vec<(String, Vec<&TargetResult>)> = by_message.into_iter().collect();
        messages.sort_by_key(|(_, group)| std::cmp::Reverse(group.len()));
        let _ = writeln!(report, "\n### {name} on {engine}: errors of choices that fail on their own, by message\n");
        let _ = writeln!(report, "| choices | message | example choice | sentence |\n|---|---|---|---|");
        for (message, group) in messages.iter().take(60) {
            let example = group.iter().min_by_key(|result| result.sentence.len()).unwrap();
            let _ = writeln!(
                report,
                "| {} | `{}` | {} | `{}` |",
                group.len(),
                message.replace('|', "\\|").replace('`', "'"),
                example.target.replace('|', "\\|"),
                example.sentence.replace('|', "\\|").replace('`', "'")
            );
        }
    }
    if names.len() > 1 {
        let _ = writeln!(report, "\n### {name}: sentences the engines answer differently ({})\n", disagreements.len());
        for disagreement in disagreements.iter().take(40) {
            let _ = writeln!(report, "- {}\n  ```\n  {}\n  ```", disagreement.target, disagreement.sentence);
            for (engine, rows) in &disagreement.answers {
                let shown: Vec<&String> = rows.iter().take(5).collect();
                let _ = writeln!(report, "  - {engine}: {} rows, first {:?}", rows.len(), shown);
            }
        }
    }
    Ok(())
}
