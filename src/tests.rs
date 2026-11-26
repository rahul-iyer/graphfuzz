//! Tests that need no engine: rendering, the oracles' rewrites, failure matching, value normalization,
//! the generator's determinism, and the bundled grammars.

use std::collections::BTreeSet;
use std::path::Path;

use crate::ast::*;
use crate::check::{Verdict, idempotent_labels, repeatable, same_failure, signature};
use crate::config::Weights;
use crate::generate::{Config, Gen};
use crate::profile::Profile;
use crate::render;
use crate::schema::SchemaConfig;
use crate::value::{Val, canonical_temporal, multiset};

fn node(var: &str, label: Option<LabelExpr>) -> NodePat {
    NodePat { var: Some(var.to_string()), label, props: vec![], filter: None }
}

fn edge(label: Option<LabelExpr>) -> EdgePat {
    EdgePat { var: None, label, dir: Dir::Right, quant: None, props: vec![], filter: None }
}

fn name(label: &str) -> LabelExpr {
    LabelExpr::Name(label.to_string())
}

/// MATCH over `paths`, RETURN count(*) AS c0.
fn counting(paths: Vec<PathPat>) -> Query {
    Query::Linear(Linear {
        clauses: vec![Clause::Match { optional: false, mode: None, paths, filter: None }],
        tail: Tail::Return(Return {
            distinct: false,
            items: ReturnItems::Items(vec![(Expr::Agg { name: "count".to_string(), distinct: false, arg: None }, Some("c0".to_string()))]),
            group_by: vec![],
            order: vec![],
            offset: None,
            limit: None,
        }),
    })
}

fn path(start: NodePat, hops: Vec<(EdgePat, NodePat)>) -> PathPat {
    PathPat { var: None, search: None, mode: None, start, hops }
}

fn profile(name: &str) -> Profile {
    Profile::load(name, Path::new("."), &[], &[], None).unwrap()
}

fn mismatch(target_rows: usize, reference_rows: usize) -> Verdict {
    Verdict::Mismatch { target: vec![], reference: vec![], target_rows, reference_rows }
}

#[test]
fn renders_one_query_in_each_dialect() {
    let query = counting(vec![path(node("a", Some(name("Person"))), vec![(edge(Some(name("KNOWS"))), node("b", None))])]);
    assert_eq!(render::gql(&query), "MATCH (a:Person)-[:KNOWS]->(b) RETURN count(*) AS c0");
    assert_eq!(render::render(&query, &profile("cypher25")), "CYPHER 25 MATCH (a:Person)-[:KNOWS]->(b) RETURN count(*) AS c0");
    assert!(render::render(&query, &profile("opencypher")).contains("MATCH (a:Person)-[:KNOWS]->(b)"));
}

#[test]
fn idempotent_labels_drop_duplicate_disjuncts() {
    let doubled = LabelExpr::Or(Box::new(name("KNOWS")), Box::new(name("KNOWS")));
    let query = counting(vec![path(node("a", None), vec![(edge(Some(doubled)), node("b", None))])]);
    let simpler = idempotent_labels(&query).expect("A|A is rewritten");
    assert_eq!(render::gql(&simpler), "MATCH (a)-[:KNOWS]->(b) RETURN count(*) AS c0");

    let distinct = LabelExpr::Or(Box::new(name("KNOWS")), Box::new(name("LIVES_IN")));
    let query = counting(vec![path(node("a", None), vec![(edge(Some(distinct)), node("b", None))])]);
    assert!(idempotent_labels(&query).is_none(), "A|B is left alone");
}

#[test]
fn repeatable_marks_only_matches_that_can_reuse_an_edge() {
    let one_hop = counting(vec![path(node("a", None), vec![(edge(None), node("b", None))])]);
    assert!(repeatable(&one_hop).is_none());

    let two_paths = counting(vec![path(node("a", None), vec![(edge(None), node("b", None))]), path(node("a", None), vec![(edge(None), node("c", None))])]);
    let variant = repeatable(&two_paths).expect("two edges in one MATCH");
    assert!(render::render(&variant, &profile("cypher25")).starts_with("CYPHER 25 MATCH REPEATABLE ELEMENTS "));
}

#[test]
fn reductions_keep_the_shape_of_a_mismatch() {
    assert!(same_failure(&mismatch(3, 1), &mismatch(41, 40)), "more rows than the reference, both times");
    assert!(!same_failure(&mismatch(3, 1), &mismatch(1, 3)), "the other side has more rows");
    assert!(!same_failure(&mismatch(1, 0), &mismatch(2, 1)), "the reference stopped being empty");
    assert!(!same_failure(&mismatch(406, 338), &Verdict::MatchMode { target_rows: 406, reference_rows: 338 }));
}

#[test]
fn error_signatures_ignore_names_and_numbers() {
    let a = signature("Binder Error: GQL binding variable 'n12' is not defined at line 3");
    let b = signature("Binder Error: GQL binding variable 'x7' is not defined at line 40");
    assert_eq!(a, b);
    assert_ne!(a, signature("Binder Error: GQL comparison operands must be scalar values"));
}

#[test]
fn values_normalize_across_engines() {
    // Seven significant digits, since Neo4j's JSON rounds doubles.
    assert_eq!(Val::Num(0.1 + 0.2).key(), Val::Num(0.3).key());
    assert_eq!(Val::Num(2.0).key(), "2");
    // Rows compare as multisets, and list order inside a value does not matter.
    let rows = vec![vec![Val::Num(1.0)], vec![Val::List(vec![Val::Num(2.0), Val::Num(1.0)])]];
    let reversed = vec![vec![Val::List(vec![Val::Num(1.0), Val::Num(2.0)])], vec![Val::Num(1.0)]];
    assert_eq!(multiset(&rows), multiset(&reversed));
    // Datetimes compare in UTC, whatever offset an engine writes.
    assert_eq!(canonical_temporal("2021-03-04T06:06:07+01:00"), canonical_temporal("2021-03-04T05:06:07Z"));
    assert_eq!(canonical_temporal("2021-03-04"), Some("2021-03-04".to_string()));
    assert_eq!(canonical_temporal("Ada"), None);
}

#[test]
fn the_generator_repeats_itself_for_a_seed_and_every_dialect_renders_it() {
    let schema = SchemaConfig::social().build(false).unwrap();
    let config = Config { exclude: BTreeSet::new(), undirected: false, isolate: None, weights: Weights::default() };
    let dialects = [profile("cypher25"), profile("opencypher")];
    let queries = |seed| {
        let mut generator = Gen::new(seed, &schema, &config);
        (0..300).map(|_| generator.generate()).collect::<Vec<_>>()
    };
    let first = queries(42);
    let again = queries(42);
    for (left, right) in first.iter().zip(&again) {
        assert_eq!(render::gql(&left.query), render::gql(&right.query));
        for dialect in &dialects {
            render::render(&left.query, dialect);
        }
    }
    let different = queries(43);
    assert!(first.iter().zip(&different).any(|(left, right)| render::gql(&left.query) != render::gql(&right.query)));
}

#[test]
fn the_bundled_grammars_parse() {
    let gql = crate::g4::Grammar::load(Path::new("grammars/gql/GQL.g4")).unwrap();
    assert!(gql.rules.contains_key("gqlProgram"));
    let cypher = crate::g4::Grammar::load(Path::new("grammars/opencypher/Cypher.g4")).unwrap();
    assert!(cypher.rules.contains_key("oC_Cypher"));
}
