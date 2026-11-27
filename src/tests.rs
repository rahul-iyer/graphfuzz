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

#[test]
fn isolating_a_feature_leaves_temporal_literals_out() {
    let schema = SchemaConfig::social().build(false).unwrap();
    for (isolate, expect_literals) in [
        ("multi_path", false),
        ("optional_match", false),
        ("insert_node", false),
        ("insert_path", false),
        ("multi_insert", false),
        ("next_set", false),
        ("temporal_literal", true),
    ] {
        let config = Config { exclude: BTreeSet::new(), undirected: false, isolate: Some(isolate.to_string()), weights: Weights::default() };
        let mut generator = Gen::new(7, &schema, &config);
        let with_literals = (0..200).filter(|_| crate::features::of(&generator.generate().query).contains("temporal_literal")).count();
        if expect_literals {
            assert!(with_literals > 0, "isolate:{isolate} should use temporal literals");
        } else {
            assert_eq!(with_literals, 0, "isolate:{isolate} produced temporal literals");
        }
    }
}

#[test]
fn random_schemas_are_valid_and_avoid_keywords() {
    // A name that is a keyword in either grammar breaks an engine's parser; GQL's PRODUCT did.
    let keywords: BTreeSet<String> = ["grammars/gql/GQL.g4", "grammars/opencypher/Cypher.g4"]
        .iter()
        .flat_map(|path| {
            std::fs::read_to_string(path).unwrap().lines().filter_map(|line| line.split(':').next().map(|name| name.trim().to_string())).collect::<Vec<_>>()
        })
        .filter(|name| !name.is_empty() && name.chars().all(|c| c.is_ascii_uppercase() || c == '_'))
        .collect();
    for seed in 0..50 {
        let file = crate::config::FileConfig::load_seeded(Some(Path::new("config.yml")), Some(seed)).unwrap();
        let schema = file.schema.build(true).unwrap().with_values(seed);
        assert!(schema.edges.iter().any(|edge| edge.source == edge.target), "seed {seed}: no self-loop edge type");
        let names = schema
            .nodes
            .iter()
            .map(|node| node.label)
            .chain(schema.edges.iter().map(|edge| edge.label))
            .chain(schema.nodes.iter().flat_map(|node| node.props.iter().map(|prop| prop.name)))
            .chain(schema.edges.iter().flat_map(|edge| edge.props.iter().map(|prop| prop.name)));
        for name in names {
            assert!(!keywords.contains(&name.to_uppercase()), "seed {seed}: {name} is a keyword");
        }
        assert!(!schema.values.is_empty());
    }
}

/// Three P nodes (1 has no age) and K edges 1->1 (a self-loop), 1->2, 2->3.
fn tiny_graph() -> crate::truth::Graph {
    use crate::schema::{Fixture, FixtureEdge, FixtureNode};
    let node = |id: i64, age: Option<i64>| {
        let mut props = vec![("id", Lit::Int(id))];
        props.extend(age.map(|age| ("age", Lit::Int(age))));
        FixtureNode { label: "P", props }
    };
    let edge = |from: i64, to: i64| FixtureEdge { label: "K", source: ("P", from), target: ("P", to), undirected: false, props: vec![] };
    let fixture = Fixture { nodes: vec![node(1, None), node(2, Some(20)), node(3, Some(30))], edges: vec![edge(1, 1), edge(1, 2), edge(2, 3)] };
    crate::truth::Graph::new(&fixture).unwrap()
}

fn clauses(query: Query) -> Vec<Clause> {
    match query {
        Query::Linear(linear) => linear.clauses,
        _ => unreachable!(),
    }
}

fn hop(dir: Dir, quant: Option<Quant>, to: NodePat) -> (EdgePat, NodePat) {
    (EdgePat { var: None, label: Some(name("K")), dir, quant, props: vec![], filter: None }, to)
}

#[test]
fn ground_truth_counts_bindings() {
    let graph = tiny_graph();
    let count = |paths: Vec<PathPat>, different: bool| graph.count(&clauses(counting(paths)), different);
    let one = |dir| vec![path(node("a", None), vec![hop(dir, None, node("b", None))])];
    assert_eq!(count(one(Dir::Right), true), Some(3));
    assert_eq!(count(one(Dir::Left), true), Some(3));
    // The self-loop is one binding either way; the other two edges are two each.
    assert_eq!(count(one(Dir::Any), true), Some(5));
    // A node variable used twice: only the self-loop.
    assert_eq!(count(vec![path(node("a", None), vec![hop(Dir::Right, None, node("a", None))])], false), Some(1));
    // Two hops: 1->1->1 uses the self-loop twice, which only REPEATABLE ELEMENTS allows.
    let two = vec![path(node("a", None), vec![hop(Dir::Right, None, node("b", None)), hop(Dir::Right, None, node("c", None))])];
    assert_eq!(count(two.clone(), false), Some(3));
    assert_eq!(count(two, true), Some(2));
    // K|K is K.
    let or = LabelExpr::Or(Box::new(name("K")), Box::new(name("K")));
    let mut doubled = one(Dir::Right);
    doubled[0].hops[0].0.label = Some(or);
    assert_eq!(count(doubled, true), Some(3));
    // {1,2}: 3 walks of one edge and 3 of two, or 2 of two without repeating the self-loop.
    let quantified = vec![path(node("a", None), vec![hop(Dir::Right, Some(Quant::Range(1, 2)), node("b", None))])];
    assert_eq!(count(quantified.clone(), false), Some(6));
    assert_eq!(count(quantified, true), Some(5));
}

#[test]
fn ground_truth_handles_optional_match_and_nulls() {
    let graph = tiny_graph();
    // Each a with no K edge to node 3 is still one row.
    let b_is_3 = Expr::Bin(BinOp::Eq, Box::new(Expr::Prop("b".to_string(), "id".to_string())), Box::new(Expr::Lit(Lit::Int(3))));
    let optional = vec![
        Clause::Match { optional: false, mode: None, paths: vec![path(node("a", Some(name("P"))), vec![])], filter: None },
        Clause::Match { optional: true, mode: None, paths: vec![path(node("a", None), vec![hop(Dir::Right, None, node("b", None))])], filter: Some(b_is_3) },
    ];
    assert_eq!(graph.count(&optional, true), Some(3));
    // Node 1 has no age: age > 10 is unknown there, so NOT (age > 10) keeps nothing more.
    let older = Expr::Bin(BinOp::Gt, Box::new(Expr::Prop("a".to_string(), "age".to_string())), Box::new(Expr::Lit(Lit::Int(10))));
    let filtered = |condition: Expr| vec![Clause::Match { optional: false, mode: None, paths: vec![path(node("a", None), vec![])], filter: Some(condition) }];
    assert_eq!(graph.count(&filtered(older.clone()), true), Some(2));
    assert_eq!(graph.count(&filtered(Expr::Not(Box::new(older))), true), Some(0));
    assert_eq!(graph.count(&filtered(Expr::IsNull(Box::new(Expr::Prop("a".to_string(), "age".to_string())), false)), true), Some(1));
    // A function it does not model: not counted.
    assert_eq!(graph.count(&filtered(Expr::Func("abs".to_string(), vec![Expr::Lit(Lit::Int(1))])), true), None);
}

#[test]
fn pattern_rewrites_keep_the_pattern() {
    let two = |quant| {
        let query = counting(vec![path(node("a", None), vec![hop(Dir::Right, None, node("b", None)), hop(Dir::Right, quant, node("c", None))])]);
        match query {
            Query::Linear(linear) => linear,
            _ => unreachable!(),
        }
    };
    let split = crate::rewrite::split_path(&two(None)).unwrap();
    assert_eq!(render::gql(&Query::Linear(split)), "MATCH (a)-[:K]->(b), (b)-[:K]->(c) RETURN count(*) AS c0");
    let parts = crate::rewrite::unroll(&two(Some(Quant::Range(1, 3)))).unwrap();
    let texts: Vec<String> = parts.into_iter().map(|part| render::gql(&Query::Linear(part))).collect();
    assert_eq!(texts[0], "MATCH (a)-[:K]->(b)-[:K]->{1}(c) RETURN count(*) AS c0");
    assert_eq!(texts.len(), 3);
    let redundant = crate::rewrite::redundant_node(&two(None)).unwrap();
    assert!(render::gql(&Query::Linear(redundant)).starts_with("MATCH (a)-[:K]->(b)-[:K]->(c), (a) "));

    let mut filtered = two(None);
    if let Clause::Match { paths, .. } = &mut filtered.clauses[0] {
        paths[0].start.props.push(("age".to_string(), Expr::Lit(Lit::Int(20))));
    }
    let inlined = crate::rewrite::inline_filter(&filtered).unwrap();
    assert_eq!(render::gql(&Query::Linear(inlined)), "MATCH (a)-[:K]->(b)-[:K]->(c) WHERE (a.age = 20) RETURN count(*) AS c0");
    if let Clause::Match { filter, .. } = &mut filtered.clauses[0] {
        *filter = Some(Expr::Bin(BinOp::Gt, Box::new(Expr::Prop("c".to_string(), "age".to_string())), Box::new(Expr::Lit(Lit::Int(1)))));
    }
    let norec = crate::rewrite::norec(&filtered).unwrap();
    assert_eq!(render::gql(&Query::Linear(norec)), "MATCH (a {age: 20})-[:K]->(b)-[:K]->(c) RETURN count(CASE WHEN (c.age > 1) THEN 1 END) AS c0");
}
