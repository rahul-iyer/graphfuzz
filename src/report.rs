//! Turns case results into a Markdown report: verdicts per engine, then for each engine its wrong answers,
//! failures grouped by error with the smallest example of each, and a feature matrix.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use crate::check::{CaseResult, Verdict, signature};
use crate::generate::FEATURES;
use crate::profile::Profile;

fn code(text: &str) -> String {
    format!("```\n{text}\n```\n")
}

fn verdict<'a>(case: &'a CaseResult, engine: &str) -> Option<&'a Verdict> {
    case.verdicts.get(engine)
}

/// The error a failed case shows, grouped by message.
fn failure_message(case: &CaseResult, engine: &str) -> Option<String> {
    match verdict(case, engine)? {
        Verdict::TargetRejects { message, .. } | Verdict::TargetOnlyError { message, .. } => Some(signature(message)),
        Verdict::Crash(detail) => Some(format!("crash: {}", signature(detail))),
        Verdict::Timeout => Some("timeout".to_string()),
        _ => None,
    }
}

fn wrong(case: &CaseResult, engine: &str) -> bool {
    matches!(verdict(case, engine), Some(Verdict::Mismatch { .. } | Verdict::Metamorphic { .. }))
}

/// The case that best shows a group: one that was reduced, else the shortest.
fn representative<'a>(cases: &[&'a CaseResult], engine: &str) -> &'a CaseResult {
    cases.iter().find(|case| case.reduced.contains_key(engine)).copied().unwrap_or_else(|| cases.iter().min_by_key(|case| case.gql.len()).unwrap())
}

/// The features a group of cases uses more often than all cases do, most over-represented first.
fn overrepresented(group: &[&CaseResult], all: &[CaseResult]) -> String {
    let mut features: BTreeSet<&str> = BTreeSet::new();
    for case in group {
        features.extend(case.tags.iter().copied());
    }
    let mut lifts: Vec<(f64, &str)> = Vec::new();
    for feature in features {
        let in_group = group.iter().filter(|case| case.tags.contains(feature)).count() as f64 / group.len() as f64;
        let overall = all.iter().filter(|case| case.tags.contains(feature)).count() as f64 / all.len() as f64;
        if in_group >= 0.5 && overall > 0.0 {
            lifts.push((in_group / overall, feature));
        }
    }
    lifts.sort_by(|left, right| right.0.partial_cmp(&left.0).unwrap());
    lifts.into_iter().take(4).map(|(lift, feature)| format!("{feature} (×{lift:.1})")).collect::<Vec<_>>().join(", ")
}

fn example(out: &mut String, case: &CaseResult, engine: &str, reference: Option<&str>) {
    if let Some(reduced) = case.reduced.get(engine) {
        let _ = writeln!(out, "Reduced (features: {}):", reduced.features.join(", "));
        out.push_str(&code(&reduced.query));
    }
    let _ = writeln!(out, "Original, as sent to {engine} (case {}, {}):", case.id, case.phase);
    out.push_str(&code(case.queries.get(engine).unwrap_or(&case.gql)));
    if let Some(reference) = reference
        && let Some(text) = case.queries.get(reference)
    {
        let _ = writeln!(out, "As sent to {reference}:");
        out.push_str(&code(text));
    }
}

fn escape(text: &str) -> String {
    text.replace('|', "\\|").replace('`', "'")
}

pub fn markdown(cases: &[CaseResult], header: &str, engines: &[String], reference: Option<(&str, &Profile)>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# graphfuzz report\n\n{header}\n");
    let reference_name = reference.map(|(name, _)| name);

    let mut names: BTreeSet<&str> = BTreeSet::new();
    for case in cases {
        names.extend(case.verdicts.values().map(Verdict::name));
    }
    let _ = writeln!(out, "## Verdicts\n\n| verdict | {} |\n|---|{}", engines.join(" | "), "---|".repeat(engines.len()));
    for name in &names {
        let counts: Vec<String> =
            engines.iter().map(|engine| cases.iter().filter(|case| verdict(case, engine).map(Verdict::name) == Some(*name)).count().to_string()).collect();
        let _ = writeln!(out, "| {name} | {} |", counts.join(" | "));
    }
    let _ = writeln!(
        out,
        "\n`agree`: same rows as the reference. `rejects`: an error where the reference answered. `unchecked_*`: the reference cannot express the case. `unsupported`: the engine's dialect cannot. `match_mode`: differs from the reference only in whether one MATCH may reuse an edge."
    );

    for engine in engines {
        engine_section(&mut out, cases, engine, reference_name);
    }

    // Every feature, and how each engine fared with it.
    let _ = writeln!(out, "\n## Features\n\nEach cell: agreed or passed / wrong / error / unsupported.\n");
    let _ = writeln!(
        out,
        "| feature | {} | {} |\n|---|---|{}",
        reference_name.map(|name| format!("in {name}?")).unwrap_or_else(|| "reference?".to_string()),
        engines.join(" | "),
        "---|".repeat(engines.len())
    );
    for (feature, description) in FEATURES {
        let using: Vec<&CaseResult> = cases.iter().filter(|case| case.tags.contains(feature)).collect();
        let in_reference = match reference {
            Some((_, profile)) if profile.unsupported.iter().any(|name| name == feature) => "no",
            Some(_) => "yes",
            None => "",
        };
        let cells: Vec<String> = engines
            .iter()
            .map(|engine| {
                let good = using.iter().filter(|case| matches!(verdict(case, engine), Some(Verdict::Agree | Verdict::TargetOnlyOk))).count();
                let bad = using.iter().filter(|case| wrong(case, engine)).count();
                let errors = using.iter().filter(|case| failure_message(case, engine).is_some()).count();
                let unsupported = using.iter().filter(|case| matches!(verdict(case, engine), Some(Verdict::Unsupported(_)))).count();
                format!("{good} / {bad} / {errors} / {unsupported}")
            })
            .collect();
        let _ = writeln!(out, "| {feature} ({}) | {in_reference} | {} |", escape(description), cells.join(" | "));
    }
    out
}

fn engine_section(out: &mut String, cases: &[CaseResult], engine: &str, reference: Option<&str>) {
    let _ = writeln!(out, "\n# {engine}\n");

    let mismatches: Vec<&CaseResult> = cases.iter().filter(|case| matches!(verdict(case, engine), Some(Verdict::Mismatch { .. }))).collect();
    let _ = writeln!(out, "## {engine}: wrong answers against {} ({})\n", reference.unwrap_or("the reference"), mismatches.len());
    if !mismatches.is_empty() {
        let _ = writeln!(out, "Over-represented features: {}\n", overrepresented(&mismatches, cases));
    }
    let mut groups: BTreeMap<String, Vec<&CaseResult>> = BTreeMap::new();
    for case in &mismatches {
        let key =
            case.reduced.get(engine).map(|reduced| reduced.features.join(", ")).unwrap_or_else(|| case.tags.iter().copied().collect::<Vec<_>>().join(", "));
        groups.entry(key).or_default().push(case);
    }
    let mut groups: Vec<(String, Vec<&CaseResult>)> = groups.into_iter().collect();
    groups.sort_by_key(|(_, group)| std::cmp::Reverse(group.len()));
    for (features, group) in groups.iter().take(25) {
        let case = representative(group, engine);
        let _ = writeln!(out, "### {} case(s): {}\n", group.len(), features);
        example(out, case, engine, reference);
        if let Some(Verdict::Mismatch { target, reference: expected, target_rows, reference_rows }) = verdict(case, engine) {
            let _ = writeln!(out, "{engine} rows ({target_rows}):");
            out.push_str(&code(&target.join("\n")));
            let _ = writeln!(out, "{} rows ({reference_rows}):", reference.unwrap_or("reference"));
            out.push_str(&code(&expected.join("\n")));
        }
    }

    let match_mode: Vec<&CaseResult> = cases.iter().filter(|case| matches!(verdict(case, engine), Some(Verdict::MatchMode { .. }))).collect();
    if !match_mode.is_empty() {
        let _ = writeln!(
            out,
            "\n## {engine}: match mode differs from {} ({})\n\nThese answers differ from {0}'s only because {engine} lets one MATCH use an edge more than once: \
             {0} returns {engine}'s rows when the MATCH says REPEATABLE ELEMENTS. Over-represented: {}\n",
            reference.unwrap_or("the reference"),
            match_mode.len(),
            overrepresented(&match_mode, cases)
        );
        let case = representative(&match_mode, engine);
        example(out, case, engine, reference);
        if let Some(Verdict::MatchMode { target_rows, reference_rows }) = verdict(case, engine) {
            let _ = writeln!(out, "{engine}: {target_rows} rows; {}: {reference_rows} rows.", reference.unwrap_or("reference"));
        }
    }

    let metamorphic: Vec<&CaseResult> = cases.iter().filter(|case| matches!(verdict(case, engine), Some(Verdict::Metamorphic { .. }))).collect();
    if !metamorphic.is_empty() {
        let _ = writeln!(out, "\n## {engine}: answers that contradict each other ({})\n", metamorphic.len());
        let mut by_oracle: BTreeMap<String, Vec<&CaseResult>> = BTreeMap::new();
        for case in &metamorphic {
            if let Some(Verdict::Metamorphic { oracle, .. }) = verdict(case, engine) {
                by_oracle.entry(oracle.clone()).or_default().push(case);
            }
        }
        for (oracle, group) in &by_oracle {
            let case = representative(group, engine);
            let _ = writeln!(out, "### {oracle}: {} case(s); over-represented: {}\n", group.len(), overrepresented(group, cases));
            example(out, case, engine, None);
            if let Some(Verdict::Metamorphic { variant, detail, .. }) = verdict(case, engine) {
                let _ = writeln!(out, "{detail}. Rewrite:");
                out.push_str(&code(variant));
            }
        }
    }

    // Features that fail on their own, over the base query.
    let mut rows = Vec::new();
    for (feature, _) in FEATURES {
        let phase = format!("isolate:{feature}");
        let isolated: Vec<&CaseResult> = cases.iter().filter(|case| case.phase == phase && case.verdicts.contains_key(engine)).collect();
        let failing: Vec<&CaseResult> = isolated.iter().copied().filter(|case| failure_message(case, engine).is_some() || wrong(case, engine)).collect();
        if failing.is_empty() {
            continue;
        }
        let shown = representative(&failing, engine);
        let error = failure_message(shown, engine).unwrap_or_else(|| verdict(shown, engine).map(Verdict::name).unwrap_or("").to_string());
        let query = shown.reduced.get(engine).map(|reduced| reduced.query.clone()).unwrap_or_else(|| shown.queries.get(engine).cloned().unwrap_or_default());
        rows.push(format!("| {feature} | {} | {} | `{}` | `{}` |", isolated.len(), failing.len(), escape(&query), escape(&error)));
    }
    if !rows.is_empty() {
        let _ = writeln!(
            out,
            "\n## {engine}: features that fail alone\n\n| feature | isolated cases | failing | smallest failing query | error |\n|---|---|---|---|---|"
        );
        for row in rows {
            let _ = writeln!(out, "{row}");
        }
    }

    let mut by_signature: BTreeMap<String, Vec<&CaseResult>> = BTreeMap::new();
    for case in cases {
        if let Some(message) = failure_message(case, engine) {
            by_signature.entry(message).or_default().push(case);
        }
    }
    let mut signatures: Vec<(String, Vec<&CaseResult>)> = by_signature.into_iter().collect();
    signatures.sort_by_key(|(_, group)| std::cmp::Reverse(group.len()));
    if !signatures.is_empty() {
        let _ = writeln!(out, "\n## {engine}: errors, by message ({} kinds)\n", signatures.len());
        let _ = writeln!(out, "| cases | reference ran it | message | over-represented features |\n|---|---|---|---|");
        for (message, group) in &signatures {
            let reference_ran = group.iter().filter(|case| matches!(verdict(case, engine), Some(Verdict::TargetRejects { .. }))).count();
            let _ = writeln!(out, "| {} | {} | `{}` | {} |", group.len(), reference_ran, escape(message), overrepresented(group, cases));
        }
        for (message, group) in signatures.iter().take(30) {
            let _ = writeln!(out, "\n### `{}`\n", escape(message));
            example(out, representative(group, engine), engine, reference);
        }
    }

    let rejected: Vec<&CaseResult> = cases.iter().filter(|case| matches!(verdict(case, engine), Some(Verdict::ReferenceRejects { .. }))).collect();
    if !rejected.is_empty() {
        let mut by_message: BTreeMap<String, Vec<&CaseResult>> = BTreeMap::new();
        for case in &rejected {
            if let Some(Verdict::ReferenceRejects { message }) = verdict(case, engine) {
                by_message.entry(signature(message)).or_default().push(case);
            }
        }
        let _ = writeln!(out, "\n## {engine}: the reference rejected what {engine} ran ({})\n", rejected.len());
        for (message, group) in &by_message {
            let _ = writeln!(out, "### {} case(s): `{}`\n", group.len(), escape(message));
            example(out, representative(group, engine), engine, reference);
        }
    }

    let both: Vec<&CaseResult> = cases.iter().filter(|case| matches!(verdict(case, engine), Some(Verdict::BothReject { .. }))).collect();
    if !both.is_empty() {
        let mut by_message: BTreeMap<String, usize> = BTreeMap::new();
        for case in &both {
            if let Some(Verdict::BothReject { target, reference }) = verdict(case, engine) {
                *by_message.entry(format!("{} / {}", signature(target), signature(reference))).or_default() += 1;
            }
        }
        let _ = writeln!(out, "\n## {engine}: both rejected ({}), usually generator noise\n", both.len());
        for (message, count) in by_message.iter().take(15) {
            let _ = writeln!(out, "- {count}: `{}`", escape(message));
        }
    }
}
