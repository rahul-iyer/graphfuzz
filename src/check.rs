//! Runs a generated case on the reference engine and on each engine under test, and decides what the
//! answers mean.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use serde::Serialize;

use crate::ast::*;
use crate::engine::{Engine, Outcome};
use crate::generate::{Generated, contains_aggregate};
use crate::profile::Profile;
use crate::render;
use crate::schema::Schema;
use crate::value::{Row, Val, multiset, sequence};

#[derive(Serialize, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ErrorKind {
    NotImplemented,
    Parser,
    Binder,
    Runtime,
    Internal,
}

/// What one engine's answer to a case means, judged against the reference engine.
#[derive(Serialize, Clone, Debug)]
pub enum Verdict {
    /// The engine and the reference returned the same rows.
    Agree,
    BothReject { target: String, reference: String },
    /// The rows differ: a correctness bug in one of the engines.
    Mismatch { target: Vec<String>, reference: Vec<String> },
    /// The engine rejected what the reference ran.
    TargetRejects { kind: ErrorKind, message: String },
    /// The reference rejected what the engine ran.
    ReferenceRejects { message: String },
    /// The reference cannot express the case; the engine ran it and passed its own checks.
    TargetOnlyOk,
    TargetOnlyError { kind: ErrorKind, message: String },
    /// A query and a rewrite of it that must have the same answer did not.
    Metamorphic { oracle: String, variant: String, detail: String },
    Crash(String),
    Timeout,
    /// Not checked, and why.
    Skipped(String),
    /// The engine's dialect lacks these features of the case.
    Unsupported(Vec<String>),
}

impl Verdict {
    pub fn name(&self) -> &'static str {
        match self {
            Verdict::Agree => "agree",
            Verdict::BothReject { .. } => "both_reject",
            Verdict::Mismatch { .. } => "mismatch",
            Verdict::TargetRejects { kind: ErrorKind::Internal, .. } | Verdict::TargetOnlyError { kind: ErrorKind::Internal, .. } => "internal_error",
            Verdict::TargetRejects { .. } => "rejects",
            Verdict::ReferenceRejects { .. } => "reference_rejects",
            Verdict::TargetOnlyOk => "unchecked_ok",
            Verdict::TargetOnlyError { .. } => "unchecked_error",
            Verdict::Metamorphic { .. } => "metamorphic",
            Verdict::Crash(_) => "crash",
            Verdict::Timeout => "timeout",
            Verdict::Skipped(_) => "skipped",
            Verdict::Unsupported(_) => "unsupported",
        }
    }
}

#[derive(Serialize, Clone)]
pub struct Reduced {
    pub query: String,
    pub features: Vec<&'static str>,
}

#[derive(Serialize, Clone)]
pub struct CaseResult {
    pub id: usize,
    pub phase: String,
    pub tags: BTreeSet<&'static str>,
    pub mutation: bool,
    /// The case in GQL, whatever engines ran it.
    pub gql: String,
    /// The text each engine was sent, by engine name.
    pub queries: BTreeMap<String, String>,
    pub verdicts: BTreeMap<String, Verdict>,
    /// The smallest query found that fails the same way, by engine name.
    pub reduced: BTreeMap<String, Reduced>,
}

pub fn classify(message: &str) -> ErrorKind {
    let lower = message.to_lowercase();
    if message.contains("INTERNAL Error") || lower.contains("internal error") || lower.contains("assertion") {
        ErrorKind::Internal
    } else if lower.contains("not implemented") || lower.contains("not supported") || lower.contains("unsupported") {
        ErrorKind::NotImplemented
    } else if lower.contains("parser") || lower.contains("syntax") {
        ErrorKind::Parser
    } else if lower.contains("binder") {
        ErrorKind::Binder
    } else {
        ErrorKind::Runtime
    }
}

/// An error message without the parts that vary between occurrences: positions, numbers, and quoted names.
pub fn signature(message: &str) -> String {
    let first = message.lines().next().unwrap_or("");
    let mut result = String::new();
    let mut chars = first.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        if let Some(open) = quote {
            if c == open {
                quote = None;
                result.push('…');
                result.push(c);
            }
            continue;
        }
        match c {
            '\'' | '`' | '"' => {
                quote = Some(c);
                result.push(c);
            }
            '0'..='9' => {
                while chars.peek().is_some_and(|next| next.is_ascii_digit()) {
                    chars.next();
                }
                result.push('#');
            }
            _ => result.push(c),
        }
    }
    let result = match result.find(" (line #, column #)") {
        Some(position) => format!("{}{}", &result[..position], &result[position + " (line #, column #)".len()..]),
        None => result,
    };
    result.chars().take(180).collect()
}

/// Whether two verdicts are the same failure: the same kind, error message, or broken rewrite.
pub fn same_failure(left: &Verdict, right: &Verdict) -> bool {
    match (left, right) {
        (Verdict::TargetRejects { message: a, .. }, Verdict::TargetRejects { message: b, .. })
        | (Verdict::TargetOnlyError { message: a, .. }, Verdict::TargetOnlyError { message: b, .. }) => signature(a) == signature(b),
        (Verdict::Metamorphic { oracle: a, .. }, Verdict::Metamorphic { oracle: b, .. }) => a == b,
        (Verdict::Mismatch { .. }, Verdict::Mismatch { .. }) | (Verdict::Timeout, Verdict::Timeout) | (Verdict::Crash(_), Verdict::Crash(_)) => true,
        _ => false,
    }
}

/// The queries that read back the whole graph after a data modification; plain Cypher and GQL alike.
pub fn digest_queries(schema: &Schema) -> Vec<String> {
    let mut queries = Vec::new();
    for node in &schema.nodes {
        let items: Vec<String> = node.props.iter().enumerate().map(|(index, prop)| format!("n.{} AS c{}", prop.name, index)).collect();
        queries.push(format!("MATCH (n:{}) RETURN {}", node.label, items.join(", ")));
    }
    for edge in &schema.edges {
        if edge.undirected {
            continue;
        }
        let mut items = vec!["a.id AS c0".to_string(), "b.id AS c1".to_string()];
        for (index, prop) in edge.props.iter().enumerate() {
            items.push(format!("e.{} AS c{}", prop.name, index + 2));
        }
        queries.push(format!("MATCH (a:{})-[e:{}]->(b:{}) RETURN {}", edge.source, edge.label, edge.target, items.join(", ")));
    }
    queries
}

/// One engine and how to talk to it.
pub struct Slot {
    pub name: String,
    pub engine: Box<dyn Engine>,
    pub profile: Profile,
    pub metamorphic: bool,
    /// Whether it holds the graph with an undirected edge type, for GQL-only exploration.
    pub undirected_graph: bool,
}

/// Whether the result's order is meaningful: ordered by every returned column, so ties are identical rows.
fn ordered(query: &Query) -> bool {
    match query {
        Query::Linear(linear) => matches!(&linear.tail, Tail::Return(ret) if !ret.order.is_empty()),
        _ => false,
    }
}

fn first_error(outcomes: &[Outcome]) -> Option<&str> {
    match outcomes.first() {
        Some(Outcome::Error(message)) => Some(message),
        _ => None,
    }
}

impl Slot {
    /// The case's own results: one outcome for a query; the modification's and each digest's for a mutation.
    fn run(&mut self, generated: &Generated, text: &str, schema: &Schema) -> Result<Vec<Outcome>> {
        if generated.mutation {
            let digests: Vec<String> = digest_queries(schema).into_iter().map(|digest| format!("{}{}", self.profile.prefix, digest)).collect();
            self.engine.rolled_back(text, &digests)
        } else {
            Ok(vec![self.engine.read(text)?])
        }
    }

    fn rows(&mut self, query: &Query) -> Option<Vec<Row>> {
        match self.engine.read(&render::render(query, &self.profile)) {
            Ok(Outcome::Rows { rows, .. }) => Some(rows),
            _ => None,
        }
    }

    /// Rewrites of a query that must return the same rows; the first that does not is the failure.
    fn metamorphic(&mut self, generated: &Generated, rows: &[Row]) -> Option<Verdict> {
        if !self.metamorphic || generated.mutation {
            return None;
        }
        let Query::Linear(linear) = &generated.query else { return None };
        let rows = rows.to_vec();
        let failure = |oracle: &str, variant: &Query, detail: String| Verdict::Metamorphic { oracle: oracle.to_string(), variant: render::gql(variant), detail };

        // Ternary logic partitioning: p, NOT p, and p IS UNKNOWN split the rows.
        if let Some(predicate) = &generated.partition {
            let unknown = Expr::Case(
                vec![(predicate.clone(), Expr::Lit(Lit::Bool(false))), (Expr::Not(Box::new(predicate.clone())), Expr::Lit(Lit::Bool(false)))],
                Some(Box::new(Expr::Lit(Lit::Bool(true)))),
            );
            let mut parts = Vec::new();
            let mut last = None;
            for condition in [predicate.clone(), Expr::Not(Box::new(predicate.clone())), unknown] {
                let mut variant = linear.clone();
                variant.clauses.push(Clause::Filter(condition));
                let variant = Query::Linear(variant);
                parts.extend(self.rows(&variant)?);
                last = Some(variant);
            }
            if multiset(&parts) != multiset(&rows) {
                return Some(failure(
                    "partition",
                    last.as_ref().unwrap(),
                    format!("{} rows without the filter, {} across p, NOT p, and UNKNOWN p", rows.len(), parts.len()),
                ));
            }
        }

        let plain = match &linear.tail {
            Tail::Return(ret) => {
                !ret.distinct
                    && ret.limit.is_none()
                    && ret.offset.is_none()
                    && match &ret.items {
                        ReturnItems::Items(items) => !items.iter().any(|(expr, _)| contains_aggregate(expr)),
                        ReturnItems::Star => false,
                    }
            }
            _ => false,
        };
        if plain {
            // count(*) counts the rows.
            let mut variant = linear.clone();
            if let Tail::Return(ret) = &mut variant.tail {
                ret.items = ReturnItems::Items(vec![(Expr::Agg { name: "count".to_string(), distinct: false, arg: None }, Some("c0".to_string()))]);
                ret.order.clear();
                ret.group_by.clear();
            }
            let variant = Query::Linear(variant);
            if let Some(counted) = self.rows(&variant) {
                let count = counted.first().and_then(|row| row.first()).cloned().unwrap_or(Val::Null);
                if count.key() != rows.len().to_string() {
                    return Some(failure("count", &variant, format!("{} rows, count(*) = {}", rows.len(), count.key())));
                }
            }
            // DISTINCT returns each row once.
            let mut variant = linear.clone();
            if let Tail::Return(ret) = &mut variant.tail {
                ret.distinct = true;
                ret.order.clear();
            }
            let variant = Query::Linear(variant);
            if let Some(distinct) = self.rows(&variant) {
                let mut expected = multiset(&rows);
                expected.dedup();
                if multiset(&distinct) != expected {
                    return Some(failure("distinct", &variant, format!("{} distinct rows, {} returned by DISTINCT", expected.len(), distinct.len())));
                }
            }
        }

        // A path read from its other end binds the same elements.
        let reversible = linear.clauses.iter().all(|clause| match clause {
            Clause::Match { paths, .. } => paths.iter().all(|path| path.var.is_none() && path.search.is_none()),
            Clause::Call { .. } => false,
            _ => true,
        });
        if reversible && linear.clauses.iter().any(|clause| matches!(clause, Clause::Match { paths, .. } if paths.iter().any(|path| !path.hops.is_empty()))) {
            let mut variant = linear.clone();
            for clause in &mut variant.clauses {
                if let Clause::Match { paths, .. } = clause {
                    for path in paths.iter_mut() {
                        *path = path.reversed();
                    }
                }
            }
            let variant = Query::Linear(variant);
            if let Some(reversed) = self.rows(&variant) {
                let same = if ordered(&generated.query) { sequence(&reversed) == sequence(&rows) } else { multiset(&reversed) == multiset(&rows) };
                if !same {
                    return Some(failure("reverse", &variant, format!("{} rows forwards, {} backwards", rows.len(), reversed.len())));
                }
            }
        }
        None
    }
}

pub struct Checker<'a> {
    pub slots: &'a mut [Slot],
    pub reference: Option<usize>,
    pub schema: &'a Schema,
}

impl<'a> Checker<'a> {
    /// Judges a case on each target engine against the reference.
    pub fn check(&mut self, id: usize, phase: &str, generated: &Generated, targets: &[usize]) -> Result<CaseResult> {
        let mut result = CaseResult {
            id,
            phase: phase.to_string(),
            tags: generated.tags.clone(),
            mutation: generated.mutation,
            gql: render::gql(&generated.query),
            queries: BTreeMap::new(),
            verdicts: BTreeMap::new(),
            reduced: BTreeMap::new(),
        };
        let reference = match self.reference {
            Some(index) if self.slots[index].profile.missing(&generated.tags).is_empty() && (!generated.mutation || self.slots[index].engine.transactional()) => {
                let slot = &mut self.slots[index];
                let text = render::render(&generated.query, &slot.profile);
                result.queries.insert(slot.name.clone(), text.clone());
                let outcomes = slot.run(generated, &text, self.schema)?;
                if let Some(Outcome::Transport(message)) = outcomes.first() {
                    for &target in targets {
                        result.verdicts.insert(self.slots[target].name.clone(), Verdict::Skipped(format!("reference unreachable: {message}")));
                    }
                    return Ok(result);
                }
                Some(outcomes)
            }
            _ => None,
        };
        for &target in targets {
            let verdict = self.judge(target, generated, reference.as_deref(), &mut result.queries)?;
            result.verdicts.insert(self.slots[target].name.clone(), verdict);
        }
        Ok(result)
    }

    /// Judges a case on one target; for reduction, which re-runs only that target and the reference.
    pub fn check_one(&mut self, target: usize, generated: &Generated) -> Result<Verdict> {
        let result = self.check(0, "reduce", generated, &[target])?;
        Ok(result.verdicts.into_values().next().unwrap_or(Verdict::Skipped("no verdict".to_string())))
    }

    fn judge(&mut self, target: usize, generated: &Generated, reference: Option<&[Outcome]>, queries: &mut BTreeMap<String, String>) -> Result<Verdict> {
        let schema = self.schema;
        let slot = &mut self.slots[target];
        let missing = slot.profile.missing(&generated.tags);
        if !missing.is_empty() {
            return Ok(Verdict::Unsupported(missing.into_iter().map(String::from).collect()));
        }
        if generated.mutation && !slot.engine.transactional() {
            return Ok(Verdict::Skipped("the connector cannot roll data modification back".to_string()));
        }
        let text = render::render(&generated.query, &slot.profile);
        queries.insert(slot.name.clone(), text.clone());
        let outcomes = slot.run(generated, &text, schema)?;
        match outcomes.first() {
            Some(Outcome::Crash(detail)) => return Ok(Verdict::Crash(detail.clone())),
            Some(Outcome::Timeout) => return Ok(Verdict::Timeout),
            Some(Outcome::Transport(message)) => return Ok(Verdict::Skipped(format!("unreachable: {message}"))),
            _ => {}
        }
        let target_error = first_error(&outcomes).map(String::from);
        let Some(reference) = reference else {
            return Ok(match target_error {
                Some(message) => Verdict::TargetOnlyError { kind: classify(&message), message },
                None => outcomes.first().and_then(Outcome::rows).cloned().and_then(|rows| slot.metamorphic(generated, &rows)).unwrap_or(Verdict::TargetOnlyOk),
            });
        };
        let reference_error = first_error(reference).map(String::from);
        Ok(match (target_error, reference_error) {
            (Some(target), Some(reference)) => Verdict::BothReject { target, reference },
            (Some(message), None) => Verdict::TargetRejects { kind: classify(&message), message },
            (None, Some(message)) => Verdict::ReferenceRejects { message },
            (None, None) => {
                // A mutation's own result is not compared; the graph it leaves is.
                let start = if generated.mutation { 1 } else { 0 };
                for index in start..outcomes.len().min(reference.len()) {
                    match (&outcomes[index], &reference[index]) {
                        (Outcome::Rows { rows: left, .. }, Outcome::Rows { rows: right, .. }) => {
                            let (left, right) = if !generated.mutation && ordered(&generated.query) {
                                (sequence(left), sequence(right))
                            } else {
                                (multiset(left), multiset(right))
                            };
                            if left != right {
                                return Ok(Verdict::Mismatch { target: left.into_iter().take(20).collect(), reference: right.into_iter().take(20).collect() });
                            }
                        }
                        (Outcome::Error(message), _) => {
                            return Ok(Verdict::TargetRejects { kind: classify(message), message: format!("digest: {message}") });
                        }
                        _ => {}
                    }
                }
                if generated.mutation {
                    Verdict::Agree
                } else {
                    outcomes.first().and_then(Outcome::rows).cloned().and_then(|rows| slot.metamorphic(generated, &rows)).unwrap_or(Verdict::Agree)
                }
            }
        })
    }
}
