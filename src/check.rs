//! Runs a generated case on both engines and decides what the answers mean.

use std::collections::BTreeSet;

use anyhow::Result;
use serde::Serialize;

use crate::ast::*;
use crate::duck::{Duck, RunOutcome, Statement, StatementResult};
use crate::generate::{Generated, contains_aggregate};
use crate::neo::Neo;
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

#[derive(Serialize, Clone, Debug)]
pub enum Verdict {
    /// Both engines returned the same rows.
    Agree,
    /// Both engines rejected the statement.
    BothReject { duck: String, neo: String },
    /// The rows differ: a correctness bug in one of the engines.
    Mismatch { duck: Vec<String>, neo: Vec<String> },
    /// DuckGQL rejected what Neo4j ran.
    DuckRejects { kind: ErrorKind, message: String },
    /// Neo4j rejected what DuckGQL ran.
    NeoRejects { message: String },
    /// A GQL-only query that DuckGQL ran and that passed its metamorphic checks.
    DuckOnlyOk,
    DuckOnlyError { kind: ErrorKind, message: String },
    /// A query and a rewrite of it that must have the same answer did not.
    Metamorphic { oracle: String, variant: String, detail: String },
    Crash(String),
    Timeout,
    /// Not checked: Neo4j could not be asked, or its answer could not be read.
    Skipped(String),
}

impl Verdict {
    pub fn name(&self) -> &'static str {
        match self {
            Verdict::Agree => "agree",
            Verdict::BothReject { .. } => "both_reject",
            Verdict::Mismatch { .. } => "mismatch",
            Verdict::DuckRejects { kind: ErrorKind::Internal, .. } | Verdict::DuckOnlyError { kind: ErrorKind::Internal, .. } => "internal_error",
            Verdict::DuckRejects { .. } => "duck_rejects",
            Verdict::NeoRejects { .. } => "neo_rejects",
            Verdict::DuckOnlyOk => "duck_only_ok",
            Verdict::DuckOnlyError { .. } => "duck_only_error",
            Verdict::Metamorphic { .. } => "metamorphic",
            Verdict::Crash(_) => "crash",
            Verdict::Timeout => "timeout",
            Verdict::Skipped(_) => "skipped",
        }
    }
}

#[derive(Serialize, Clone)]
pub struct CaseResult {
    pub id: usize,
    pub phase: String,
    pub tags: BTreeSet<&'static str>,
    pub mutation: bool,
    pub gql: String,
    pub cypher: Option<String>,
    pub verdict: Verdict,
    /// The smallest query found that fails the same way, and its features.
    pub reduced: Option<String>,
    pub reduced_features: Vec<&'static str>,
}

/// Whether two verdicts are the same failure: the same kind, error message, or broken rewrite.
pub fn same_failure(left: &Verdict, right: &Verdict) -> bool {
    match (left, right) {
        (Verdict::DuckRejects { message: a, .. }, Verdict::DuckRejects { message: b, .. })
        | (Verdict::DuckOnlyError { message: a, .. }, Verdict::DuckOnlyError { message: b, .. }) => signature(a) == signature(b),
        (Verdict::Metamorphic { oracle: a, .. }, Verdict::Metamorphic { oracle: b, .. }) => a == b,
        (Verdict::Mismatch { .. }, Verdict::Mismatch { .. }) | (Verdict::Timeout, Verdict::Timeout) | (Verdict::Crash(_), Verdict::Crash(_)) => true,
        _ => false,
    }
}

pub fn classify(message: &str) -> ErrorKind {
    if message.contains("INTERNAL Error") {
        ErrorKind::Internal
    } else if message.contains("not implemented") || message.contains("Not implemented") {
        ErrorKind::NotImplemented
    } else if message.contains("Parser Error") || message.contains("parser error") {
        ErrorKind::Parser
    } else if message.contains("Binder Error") {
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

/// The queries that read back the whole graph after a data modification.
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

pub struct Checker<'a> {
    pub duck: &'a mut Duck,
    pub neo: Option<&'a Neo>,
    pub schema: &'a Schema,
    /// The DuckGQL graph the queries run on.
    pub graph: &'a str,
    pub metamorphic: bool,
}

fn rows_of(result: &StatementResult) -> Option<&Vec<Row>> {
    match result {
        StatementResult::Rows { rows, .. } => Some(rows),
        _ => None,
    }
}

fn error_of(result: &StatementResult) -> Option<&str> {
    match result {
        StatementResult::Error(message) => Some(message),
        _ => None,
    }
}

/// Whether the result's order is meaningful: ordered by every returned column, so ties are identical rows.
fn ordered(query: &Query) -> bool {
    match query {
        Query::Linear(linear) => matches!(&linear.tail, Tail::Return(ret) if !ret.order.is_empty()),
        _ => false,
    }
}

impl<'a> Checker<'a> {
    fn duck_statements(&self, gql: &str, mutation: bool) -> Vec<Statement> {
        let mut statements = vec![Statement { sql: format!("SESSION SET GRAPH {}", self.graph), read: false }];
        if mutation {
            statements.push(Statement { sql: "BEGIN".to_string(), read: false });
            statements.push(Statement { sql: gql.to_string(), read: false });
            for digest in digest_queries(self.schema) {
                statements.push(Statement { sql: digest, read: true });
            }
            statements.push(Statement { sql: "ROLLBACK".to_string(), read: false });
        } else {
            statements.push(Statement { sql: gql.to_string(), read: true });
        }
        statements
    }

    /// The query's own results on DuckGQL, without the session and transaction statements around it.
    fn run_duck(&mut self, gql: &str, mutation: bool) -> Result<Result<Vec<StatementResult>, Verdict>> {
        let statements = self.duck_statements(gql, mutation);
        Ok(match self.duck.run(&statements)? {
            RunOutcome::Results(results) => {
                if mutation {
                    // The session statement and BEGIN come first, then the mutation and the digests.
                    let mut own = vec![results[2].clone()];
                    own.extend(results[3..results.len() - 1].iter().cloned());
                    Ok(own)
                } else {
                    Ok(vec![results[1].clone()])
                }
            }
            RunOutcome::Crash(detail) => Err(Verdict::Crash(detail)),
            RunOutcome::Timeout => Err(Verdict::Timeout),
        })
    }

    fn run_neo(&self, cypher: &str, mutation: bool) -> Result<Vec<StatementResult>> {
        let neo = self.neo.unwrap();
        if mutation {
            let mut statements = vec![cypher.to_string()];
            statements.extend(digest_queries(self.schema).into_iter().map(|digest| format!("CYPHER 25 {digest}")));
            neo.run_rolled_back(&statements)
        } else {
            Ok(vec![neo.run(cypher)?])
        }
    }

    pub fn check(&mut self, id: usize, phase: &str, generated: &Generated) -> Result<CaseResult> {
        let gql = render::gql(&generated.query);
        let cypher = if self.neo.is_some() { render::cypher(&generated.query) } else { None };
        let mutation = generated.mutation;
        let make = |verdict: Verdict| CaseResult {
            id,
            phase: phase.to_string(),
            tags: generated.tags.clone(),
            mutation,
            gql: gql.clone(),
            cypher: cypher.clone(),
            verdict,
            reduced: None,
            reduced_features: Vec::new(),
        };
        let duck = match self.run_duck(&gql, mutation)? {
            Ok(results) => results,
            Err(verdict) => return Ok(make(verdict)),
        };
        let duck_error = error_of(&duck[0]).map(String::from);
        let Some(cypher_text) = cypher.clone() else {
            let verdict = match duck_error {
                Some(message) => Verdict::DuckOnlyError { kind: classify(&message), message },
                None => self.metamorphic(generated, &duck).unwrap_or(Verdict::DuckOnlyOk),
            };
            return Ok(make(verdict));
        };
        let neo = self.run_neo(&cypher_text, mutation)?;
        let neo_error = error_of(&neo[0]).map(String::from);
        if let Some(message) = neo_error.as_deref().filter(|message| message.starts_with(crate::neo::TRANSPORT_ERROR)) {
            return Ok(make(Verdict::Skipped(message.to_string())));
        }
        let verdict = match (duck_error, neo_error) {
            (Some(duck), Some(neo)) => Verdict::BothReject { duck, neo },
            (Some(message), None) => Verdict::DuckRejects { kind: classify(&message), message },
            (None, Some(message)) => Verdict::NeoRejects { message },
            (None, None) => {
                let compare = |left: &[Row], right: &[Row]| {
                    if !mutation && ordered(&generated.query) { (sequence(left), sequence(right)) } else { (multiset(left), multiset(right)) }
                };
                let mut verdict = Verdict::Agree;
                // A mutation's own result is not compared; the graph it leaves is.
                let start = if mutation { 1 } else { 0 };
                for index in start..duck.len().min(neo.len()) {
                    let (Some(duck_rows), Some(neo_rows)) = (rows_of(&duck[index]), rows_of(&neo[index])) else {
                        if let Some(message) = error_of(&duck[index]) {
                            verdict = Verdict::DuckRejects { kind: classify(message), message: format!("digest: {message}") };
                        }
                        break;
                    };
                    let (left, right) = compare(duck_rows, neo_rows);
                    if left != right {
                        verdict = Verdict::Mismatch { duck: left.into_iter().take(20).collect(), neo: right.into_iter().take(20).collect() };
                        break;
                    }
                }
                if matches!(verdict, Verdict::Agree) && !mutation {
                    if let Some(failure) = self.metamorphic(generated, &duck) {
                        verdict = failure;
                    }
                }
                verdict
            }
        };
        Ok(make(verdict))
    }

    fn duck_rows(&mut self, query: &Query) -> Result<Option<Vec<Row>>> {
        let gql = render::gql(query);
        Ok(match self.run_duck(&gql, false)? {
            Ok(results) => rows_of(&results[0]).cloned(),
            Err(_) => None,
        })
    }

    /// Rewrites of a query that must return the same rows on DuckGQL; the first that does not is the failure.
    fn metamorphic(&mut self, generated: &Generated, duck: &[StatementResult]) -> Option<Verdict> {
        if !self.metamorphic || generated.mutation {
            return None;
        }
        let rows = rows_of(&duck[0])?.clone();
        let Query::Linear(linear) = &generated.query else { return None };
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
                match self.duck_rows(&variant).ok().flatten() {
                    Some(part) => parts.extend(part),
                    None => return None,
                }
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

        // count(*) counts the rows.
        if plain {
            let mut variant = linear.clone();
            if let Tail::Return(ret) = &mut variant.tail {
                ret.items = ReturnItems::Items(vec![(Expr::Agg { name: "count".to_string(), distinct: false, arg: None }, Some("c0".to_string()))]);
                ret.order.clear();
                ret.group_by.clear();
            }
            let variant = Query::Linear(variant);
            if let Some(counted) = self.duck_rows(&variant).ok().flatten() {
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
            if let Some(distinct) = self.duck_rows(&variant).ok().flatten() {
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
            if let Some(reversed) = self.duck_rows(&variant).ok().flatten() {
                let same = if ordered(&generated.query) { sequence(&reversed) == sequence(&rows) } else { multiset(&reversed) == multiset(&rows) };
                if !same {
                    return Some(failure("reverse", &variant, format!("{} rows forwards, {} backwards", rows.len(), reversed.len())));
                }
            }
        }
        None
    }
}
