//! Turns case results into a Markdown report: verdict totals, failures grouped by error signature with the
//! shortest example of each, and a per-feature matrix.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use crate::check::{CaseResult, Verdict, signature};
use crate::generate::{FEATURES, GQL_ONLY};

/// The query that best shows a group: the shortest.
fn shortest<'a>(cases: &[&'a CaseResult]) -> &'a CaseResult {
    cases.iter().min_by_key(|case| case.gql.len()).unwrap()
}

fn failure_message(case: &CaseResult) -> Option<String> {
    match &case.verdict {
        Verdict::DuckRejects { message, .. } | Verdict::DuckOnlyError { message, .. } => Some(signature(message)),
        Verdict::Crash(detail) => Some(format!("crash: {}", signature(detail))),
        Verdict::Timeout => Some("timeout".to_string()),
        _ => None,
    }
}

/// The features a group of cases uses more often than all cases do, most over-represented first.
fn overrepresented(group: &[&CaseResult], all: &[CaseResult]) -> Vec<String> {
    let mut lifts: Vec<(f64, &str)> = Vec::new();
    let mut features: BTreeSet<&str> = BTreeSet::new();
    for case in group {
        features.extend(case.tags.iter().copied());
    }
    for feature in features {
        let in_group = group.iter().filter(|case| case.tags.contains(feature)).count() as f64 / group.len() as f64;
        let overall = all.iter().filter(|case| case.tags.contains(feature)).count() as f64 / all.len() as f64;
        if in_group >= 0.5 && overall > 0.0 {
            lifts.push((in_group / overall, feature));
        }
    }
    lifts.sort_by(|left, right| right.0.partial_cmp(&left.0).unwrap());
    lifts.into_iter().take(4).map(|(lift, feature)| format!("{feature} (×{lift:.1})")).collect()
}

fn code(text: &str) -> String {
    format!("```\n{}\n```\n", text)
}

fn example(out: &mut String, case: &CaseResult) {
    if let Some(reduced) = &case.reduced {
        let _ = writeln!(out, "Reduced (features: {}):", case.reduced_features.join(", "));
        out.push_str(&code(reduced));
    }
    let _ = writeln!(out, "Original GQL (case {}, {}):", case.id, case.phase);
    out.push_str(&code(&case.gql));
    if let Some(cypher) = &case.cypher {
        let _ = writeln!(out, "Cypher:");
        out.push_str(&code(cypher));
    }
}

pub fn markdown(cases: &[CaseResult], header: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# graphfuzz report\n\n{header}\n");

    let mut totals: BTreeMap<&str, usize> = BTreeMap::new();
    for case in cases {
        *totals.entry(case.verdict.name()).or_default() += 1;
    }
    let _ = writeln!(out, "## Verdicts\n\n| verdict | cases |\n|---|---|");
    for (name, count) in &totals {
        let _ = writeln!(out, "| {name} | {count} |");
    }

    // Correctness first: wrong answers, crashes, internal errors.
    let mismatches: Vec<&CaseResult> = cases.iter().filter(|case| matches!(case.verdict, Verdict::Mismatch { .. })).collect();
    let _ = writeln!(out, "\n## Wrong answers: DuckGQL and Neo4j disagree ({})\n", mismatches.len());
    let mut by_features: BTreeMap<String, Vec<&CaseResult>> = BTreeMap::new();
    for case in &mismatches {
        let key = case.tags.iter().copied().collect::<Vec<_>>().join(", ");
        by_features.entry(key).or_default().push(case);
    }
    let mut groups: Vec<(&String, &Vec<&CaseResult>)> = by_features.iter().collect();
    groups.sort_by_key(|(_, group)| std::cmp::Reverse(group.len()));
    let _ = writeln!(out, "Features over-represented among wrong answers: {}\n", overrepresented(&mismatches, cases).join(", "));
    for (_, group) in groups.iter().take(25) {
        let case = shortest(group);
        let _ = writeln!(out, "### {} case(s) using: {}\n", group.len(), case.tags.iter().copied().collect::<Vec<_>>().join(", "));
        example(&mut out, case);
        if let Verdict::Mismatch { duck, neo } = &case.verdict {
            let _ = writeln!(out, "DuckGQL rows:");
            out.push_str(&code(&duck.join("\n")));
            let _ = writeln!(out, "Neo4j rows:");
            out.push_str(&code(&neo.join("\n")));
        }
    }

    let metamorphic: Vec<&CaseResult> = cases.iter().filter(|case| matches!(case.verdict, Verdict::Metamorphic { .. })).collect();
    let _ = writeln!(out, "\n## Self-inconsistent DuckGQL answers ({})\n", metamorphic.len());
    let mut by_oracle: BTreeMap<String, Vec<&CaseResult>> = BTreeMap::new();
    for case in &metamorphic {
        if let Verdict::Metamorphic { oracle, .. } = &case.verdict {
            by_oracle.entry(oracle.clone()).or_default().push(case);
        }
    }
    for (oracle, group) in &by_oracle {
        let case = shortest(group);
        let _ = writeln!(out, "### {oracle}: {} case(s); over-represented: {}\n", group.len(), overrepresented(group, cases).join(", "));
        example(&mut out, case);
        if let Verdict::Metamorphic { variant, detail, .. } = &case.verdict {
            let _ = writeln!(out, "{detail}. Rewrite:");
            out.push_str(&code(variant));
        }
    }

    // Features that fail on their own, over the base query.
    let _ = writeln!(out, "\n## Features that fail alone\n\n| feature | Neo4j? | isolated cases | failing | minimal failing query | error |\n|---|---|---|---|---|---|");
    for (feature, _) in FEATURES {
        let phase = format!("isolate:{feature}");
        let isolated: Vec<&CaseResult> = cases.iter().filter(|case| case.phase == phase).collect();
        let failing: Vec<&&CaseResult> = isolated.iter().filter(|case| failure_message(case).is_some() || matches!(case.verdict, Verdict::Mismatch { .. } | Verdict::Metamorphic { .. })).collect();
        if failing.is_empty() {
            continue;
        }
        let shown = failing.iter().find(|case| case.reduced.is_some()).unwrap_or(&failing[0]);
        let error = failure_message(shown).unwrap_or_else(|| shown.verdict.name().to_string());
        let _ = writeln!(
            out,
            "| {feature} | {} | {} | {} | `{}` | `{}` |",
            if GQL_ONLY.contains(feature) { "no" } else { "yes" },
            isolated.len(),
            failing.len(),
            shown.reduced.as_deref().unwrap_or(&shown.gql).replace('|', "\\|"),
            error.replace('|', "\\|")
        );
    }

    // Then what DuckGQL cannot run, grouped by error.
    let mut by_signature: BTreeMap<String, Vec<&CaseResult>> = BTreeMap::new();
    for case in cases {
        if let Some(message) = failure_message(case) {
            by_signature.entry(message).or_default().push(case);
        }
    }
    let mut signatures: Vec<(&String, &Vec<&CaseResult>)> = by_signature.iter().collect();
    signatures.sort_by_key(|(_, group)| std::cmp::Reverse(group.len()));
    let _ = writeln!(out, "\n## DuckGQL errors, by message ({} kinds)\n", signatures.len());
    let _ = writeln!(out, "| cases | Neo4j ran it | message | over-represented features |\n|---|---|---|---|");
    for (message, group) in &signatures {
        let neo_ran = group.iter().filter(|case| matches!(case.verdict, Verdict::DuckRejects { .. })).count();
        let _ = writeln!(out, "| {} | {} | `{}` | {} |", group.len(), neo_ran, message.replace('|', "\\|"), overrepresented(group, cases).join(", "));
    }
    for (message, group) in signatures.iter().take(40) {
        let _ = writeln!(out, "\n### `{}`\n", message.replace('`', "'"));
        example(&mut out, shortest(group));
    }

    // Disagreements that are more likely the generator's or Neo4j's.
    let neo_rejects: Vec<&CaseResult> = cases.iter().filter(|case| matches!(case.verdict, Verdict::NeoRejects { .. })).collect();
    let mut neo_signatures: BTreeMap<String, Vec<&CaseResult>> = BTreeMap::new();
    for case in &neo_rejects {
        if let Verdict::NeoRejects { message } = &case.verdict {
            neo_signatures.entry(signature(message)).or_default().push(case);
        }
    }
    let _ = writeln!(out, "\n## Neo4j rejected what DuckGQL ran ({})\n", neo_rejects.len());
    for (message, group) in &neo_signatures {
        let _ = writeln!(out, "### {} case(s): `{}`\n", group.len(), message.replace('`', "'"));
        example(&mut out, shortest(group));
    }
    let both: Vec<&CaseResult> = cases.iter().filter(|case| matches!(case.verdict, Verdict::BothReject { .. })).collect();
    let mut both_signatures: BTreeMap<String, Vec<&CaseResult>> = BTreeMap::new();
    for case in &both {
        if let Verdict::BothReject { duck, neo } = &case.verdict {
            both_signatures.entry(format!("{} / {}", signature(duck), signature(neo))).or_default().push(case);
        }
    }
    let _ = writeln!(out, "\n## Both engines rejected ({})\n", both.len());
    for (message, group) in both_signatures.iter().take(15) {
        let _ = writeln!(out, "- {} case(s): `{}` e.g. `{}`", group.len(), message.replace('`', "'"), shortest(group).gql.replace('`', "'"));
    }

    // Every feature: how often it ran and how it fared.
    let _ = writeln!(out, "\n## Features\n\n| feature | Neo4j? | cases | agree/ok | wrong | DuckGQL error | most common DuckGQL error |\n|---|---|---|---|---|---|---|");
    for (feature, description) in FEATURES {
        let using: Vec<&CaseResult> = cases.iter().filter(|case| case.tags.contains(feature)).collect();
        if using.is_empty() {
            let _ = writeln!(out, "| {feature} ({description}) | | 0 | | | | |");
            continue;
        }
        let good = using.iter().filter(|case| matches!(case.verdict, Verdict::Agree | Verdict::DuckOnlyOk)).count();
        let wrong = using.iter().filter(|case| matches!(case.verdict, Verdict::Mismatch { .. } | Verdict::Metamorphic { .. })).count();
        let errors: Vec<&&CaseResult> = using.iter().filter(|case| failure_message(case).is_some()).collect();
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for case in &errors {
            *counts.entry(failure_message(case).unwrap()).or_default() += 1;
        }
        let common = counts.iter().max_by_key(|(_, count)| **count).map(|(message, _)| message.replace('|', "\\|")).unwrap_or_default();
        let _ = writeln!(
            out,
            "| {feature} ({description}) | {} | {} | {} | {} | {} | `{}` |",
            if GQL_ONLY.contains(feature) { "no" } else { "yes" },
            using.len(),
            good,
            wrong,
            errors.len(),
            common
        );
    }
    out
}
