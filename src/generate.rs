//! Random query generation. Every optional construct is a named feature: the generator records the
//! features each query uses, and an isolation run turns on one feature at a time over a fixed base.

use std::collections::BTreeSet;

use crate::ast::*;
use crate::config::Weights;
use crate::features;
use crate::rng::Rng;
use crate::schema::{Schema, Ty};

/// Every feature the generator can turn on. GQL-only features have no Neo4j counterpart, so queries using
/// them are checked against DuckGQL alone.
pub const FEATURES: &[(&str, &str)] = &[
    // Patterns
    ("multi_path", "two comma-separated paths in one MATCH"),
    ("multi_match", "two MATCH statements"),
    ("optional_match", "OPTIONAL MATCH"),
    ("two_hops", "a path of two edges"),
    ("edge_left", "a <-[ ]- edge"),
    ("edge_any", "a -[ ]- edge"),
    ("edge_left_or_right", "a <-[ ]-> edge"),
    ("edge_undirected", "a ~[ ]~ edge"),
    ("edge_left_or_undirected", "a <~[ ]~ edge"),
    ("edge_undirected_or_right", "a ~[ ]~> edge"),
    ("abbreviated_edge", "an edge without brackets, as -> or -"),
    ("unlabeled_node", "a node without a label"),
    ("unlabeled_edge", "an edge without a label"),
    ("edge_var", "a named edge"),
    ("label_or", "label disjunction A|B"),
    ("label_not", "label negation !A"),
    ("label_and", "label conjunction A&B"),
    ("label_wildcard", "the label wildcard %"),
    ("property_map", "a property map in a pattern, as (n {id: 1})"),
    ("element_where", "a WHERE inside an element pattern"),
    ("quant_fixed", "an edge quantifier {n}"),
    ("quant_range", "an edge quantifier {m,n}"),
    ("quant_plus", "the + quantifier"),
    ("quant_star", "the * quantifier"),
    ("path_var", "a path variable p = ..."),
    ("any_shortest", "ANY SHORTEST"),
    ("all_shortest", "ALL SHORTEST"),
    ("shortest_k", "SHORTEST k"),
    ("shortest_k_groups", "SHORTEST k GROUPS"),
    ("any_k", "ANY k"),
    ("path_trail", "the TRAIL path mode"),
    ("path_acyclic", "the ACYCLIC path mode"),
    ("path_simple", "the SIMPLE path mode"),
    ("path_walk", "the WALK path mode"),
    ("match_repeatable", "REPEATABLE ELEMENTS"),
    ("match_different", "DIFFERENT EDGES"),
    // Statements
    ("let", "LET"),
    ("for", "FOR x IN list"),
    ("for_ordinality", "FOR ... WITH ORDINALITY"),
    ("for_offset", "FOR ... WITH OFFSET"),
    ("filter", "FILTER"),
    ("order_page_mid", "ORDER BY / OFFSET / LIMIT before RETURN"),
    ("inline_call", "CALL (vars) { ... }"),
    ("optional_call", "OPTIONAL CALL"),
    ("call_no_scope", "CALL { ... } importing every variable"),
    ("matchless", "a query that starts without MATCH"),
    ("next", "NEXT"),
    ("union", "UNION"),
    ("union_all", "UNION ALL"),
    ("except", "EXCEPT"),
    ("intersect", "INTERSECT"),
    ("otherwise", "OTHERWISE"),
    ("finish", "FINISH"),
    // Results
    ("return_distinct", "RETURN DISTINCT"),
    ("return_star", "RETURN *"),
    ("group_by", "GROUP BY"),
    ("order_by", "ORDER BY"),
    ("order_desc", "ORDER BY ... DESC"),
    ("limit", "LIMIT"),
    ("offset", "OFFSET"),
    ("agg_count", "count(*)"),
    ("agg_count_expr", "count(expr)"),
    ("agg_count_distinct", "count(DISTINCT expr)"),
    ("agg_sum", "sum"),
    ("agg_avg", "avg"),
    ("agg_min_max", "min and max"),
    ("agg_collect", "collect_list"),
    // Expressions
    ("arith", "+, -, *"),
    ("div", "/ between numbers"),
    ("neg", "unary minus"),
    ("concat", "||"),
    ("or", "OR"),
    ("not", "NOT"),
    ("xor", "XOR"),
    ("is_null", "IS [NOT] NULL"),
    ("case", "searched CASE"),
    ("simple_case", "simple CASE"),
    ("coalesce", "COALESCE"),
    ("nullif", "NULLIF"),
    ("cast", "CAST"),
    ("string_fn", "UPPER, LOWER, TRIM, LEFT, RIGHT"),
    ("char_length", "CHAR_LENGTH"),
    ("numeric_fn", "ABS, CEIL, FLOOR, SQRT"),
    ("mod", "MOD"),
    ("power", "POWER"),
    ("trig", "SIN, COS"),
    ("log_fn", "LN, LOG10, EXP"),
    ("list", "list constructors"),
    ("size", "SIZE of a list"),
    ("path_length", "PATH_LENGTH"),
    ("element_id", "ELEMENT_ID equality"),
    ("is_labeled", "IS LABELED"),
    ("is_directed", "IS DIRECTED"),
    ("source_of", "IS SOURCE OF / IS DESTINATION OF"),
    ("all_different", "ALL_DIFFERENT"),
    ("same", "SAME"),
    ("property_exists", "PROPERTY_EXISTS"),
    ("is_typed", "IS TYPED"),
    ("is_normalized", "IS NORMALIZED"),
    ("exists_pattern", "EXISTS { pattern }"),
    ("exists_match", "EXISTS { MATCH ... }"),
    ("float_literal", "approximate number literals"),
    ("string_literal", "string literals"),
    ("temporal_literal", "DATE and DATETIME literals"),
    ("decimal_literal", "exact decimal literals"),
    // Data modification
    ("insert_node", "INSERT of a node"),
    ("insert_path", "INSERT of a path"),
    ("match_insert", "MATCH ... INSERT"),
    ("multi_insert", "several INSERT statements"),
    ("set_prop", "SET n.p = v"),
    ("remove_prop", "REMOVE n.p"),
    ("delete_edge", "DELETE of an edge"),
    ("detach_delete", "DETACH DELETE"),
    ("delete_attached", "DELETE of a node with edges"),
    ("for_insert", "FOR ... INSERT"),
    ("next_set", "... NEXT SET"),
];

/// Named sets of features, so a configuration can weigh, focus, or disable several at once.
pub const GROUPS: &[(&str, &[&str])] = &[
    (
        "patterns",
        &[
            "multi_path",
            "multi_match",
            "two_hops",
            "edge_left",
            "edge_any",
            "edge_left_or_right",
            "unlabeled_node",
            "unlabeled_edge",
            "edge_var",
            "property_map",
            "element_where",
        ],
    ),
    ("directions", &["edge_left", "edge_any", "edge_left_or_right", "edge_undirected", "edge_left_or_undirected", "edge_undirected_or_right"]),
    ("undirected", &["edge_undirected", "edge_left_or_undirected", "edge_undirected_or_right"]),
    ("labels", &["label_or", "label_not", "label_and", "label_wildcard", "unlabeled_node", "unlabeled_edge"]),
    ("quantifiers", &["quant_fixed", "quant_range", "quant_plus", "quant_star"]),
    ("path_search", &["any_shortest", "all_shortest", "shortest_k", "shortest_k_groups", "any_k"]),
    ("path_modes", &["path_trail", "path_acyclic", "path_simple", "path_walk"]),
    ("match_modes", &["match_repeatable", "match_different"]),
    (
        "paths",
        &[
            "quant_fixed",
            "quant_range",
            "quant_plus",
            "quant_star",
            "path_var",
            "any_shortest",
            "all_shortest",
            "shortest_k",
            "shortest_k_groups",
            "any_k",
            "path_trail",
            "path_acyclic",
            "path_simple",
            "path_walk",
            "path_length",
        ],
    ),
    ("optional", &["optional_match", "optional_call"]),
    ("statements", &["let", "for", "for_ordinality", "for_offset", "filter", "order_page_mid", "inline_call", "call_no_scope", "matchless", "next", "finish"]),
    ("calls", &["inline_call", "optional_call", "call_no_scope"]),
    ("composite", &["union", "union_all", "except", "intersect", "otherwise"]),
    ("results", &["return_distinct", "return_star", "group_by", "order_by", "order_desc", "limit", "offset"]),
    ("paging", &["order_by", "order_desc", "limit", "offset", "order_page_mid"]),
    ("aggregates", &["agg_count", "agg_count_expr", "agg_count_distinct", "agg_sum", "agg_avg", "agg_min_max", "agg_collect"]),
    (
        "functions",
        &["coalesce", "nullif", "cast", "string_fn", "char_length", "numeric_fn", "mod", "power", "trig", "log_fn", "size", "path_length", "element_id"],
    ),
    ("expressions", &["arith", "div", "neg", "concat", "or", "not", "xor", "is_null", "case", "simple_case", "list", "float_literal", "string_literal"]),
    (
        "predicates",
        &[
            "is_null",
            "is_labeled",
            "is_directed",
            "source_of",
            "all_different",
            "same",
            "property_exists",
            "is_typed",
            "is_normalized",
            "exists_pattern",
            "exists_match",
        ],
    ),
    ("exists", &["exists_pattern", "exists_match"]),
    ("types", &["temporal_literal", "decimal_literal", "float_literal", "string_literal", "is_typed"]),
    (
        "mutations",
        &[
            "insert_node",
            "insert_path",
            "match_insert",
            "multi_insert",
            "set_prop",
            "remove_prop",
            "delete_edge",
            "detach_delete",
            "delete_attached",
            "for_insert",
            "next_set",
        ],
    ),
];

/// The aggregates; an aggregating RETURN uses one.
const AGGREGATES: &[&str] = &["agg_count", "agg_count_expr", "agg_count_distinct", "agg_sum", "agg_avg", "agg_min_max", "agg_collect"];

/// The data-modifying features; a random case that modifies data uses exactly one.
const MUTATIONS: &[&str] = &[
    "insert_node",
    "insert_path",
    "match_insert",
    "multi_insert",
    "set_prop",
    "remove_prop",
    "delete_edge",
    "detach_delete",
    "delete_attached",
    "for_insert",
    "next_set",
];

/// Features whose answers ISO GQL leaves to the implementation, so engines may differ without either being
/// wrong: exact division, and deleting a node that still has edges (checked at commit by some engines).
pub const IMPLEMENTATION_DEFINED: &[&str] = &["div", "delete_attached"];

#[derive(Clone, Debug)]
pub enum Kind {
    Node(Option<&'static str>),
    Edge(Option<&'static str>),
    Path,
    Value(Ty),
    #[allow(dead_code)] // the element type, for typed list expressions to come
    List(Ty),
}

#[derive(Clone, Debug)]
pub struct Var {
    pub name: String,
    pub kind: Kind,
}

pub struct Config {
    /// Features never generated: those the reference cannot express or that are implementation-defined.
    pub exclude: BTreeSet<&'static str>,
    pub undirected: bool,
    /// Turn on only this feature, over the base every query uses.
    pub isolate: Option<String>,
    /// How often each feature is used, from the configuration file.
    pub weights: Weights,
}

/// One generated query and what the runner needs to check it.
#[derive(Clone)]
pub struct Generated {
    pub query: Query,
    pub tags: BTreeSet<&'static str>,
    pub mutation: bool,
    /// A predicate over the final scope, for the ternary logic partitioning oracle.
    pub partition: Option<Expr>,
}

pub struct Gen<'a> {
    rng: Rng,
    schema: &'a Schema,
    config: &'a Config,
    tags: BTreeSet<&'static str>,
    scope: Vec<Var>,
    next_name: usize,
    next_id: i64,
    /// The columns the last RETURN produced, for the statement after NEXT.
    returned: Vec<Var>,
    /// Inside EXISTS, where neither engine takes path variables or path searches.
    in_exists: bool,
}

fn feature_name(name: &str) -> &'static str {
    FEATURES.iter().find(|(feature, _)| *feature == name).map(|(feature, _)| *feature).unwrap_or_else(|| panic!("unknown feature {name}"))
}

impl<'a> Gen<'a> {
    pub fn new(seed: u64, schema: &'a Schema, config: &'a Config) -> Gen<'a> {
        Gen {
            rng: Rng::new(seed),
            schema,
            config,
            tags: BTreeSet::new(),
            scope: Vec::new(),
            next_name: 0,
            next_id: 1000,
            returned: Vec::new(),
            in_exists: false,
        }
    }

    /// Whether to use a feature here. In an isolation run only the isolated feature is ever used.
    fn want(&mut self, feature: &str, probability: f64) -> bool {
        let feature = feature_name(feature);
        if self.config.exclude.contains(feature) {
            return false;
        }
        if !self.config.undirected && feature.contains("undirected") {
            return false;
        }
        let weights = &self.config.weights;
        let yes = match &self.config.isolate {
            Some(target) => target == feature,
            None if weights.disabled.contains(feature) => false,
            None if weights.focus.contains(feature) => true,
            None => self.rng.chance((probability * weights.multiplier(feature)).min(1.0)),
        };
        if yes {
            self.tags.insert(feature);
        }
        yes
    }

    fn fresh(&mut self, prefix: &str) -> String {
        self.next_name += 1;
        format!("{prefix}{}", self.next_name)
    }

    pub fn generate(&mut self) -> Generated {
        self.tags.clear();
        self.scope.clear();
        let mutation_features = [
            "insert_node",
            "insert_path",
            "match_insert",
            "multi_insert",
            "set_prop",
            "remove_prop",
            "delete_edge",
            "detach_delete",
            "delete_attached",
            "for_insert",
            "next_set",
        ];
        let isolated_mutation = self.config.isolate.as_deref().map(|target| mutation_features.contains(&target)).unwrap_or(false);
        let weights = &self.config.weights;
        let focused_mutation = MUTATIONS.iter().any(|kind| weights.focus.contains(kind));
        let allowed_mutation = MUTATIONS.iter().any(|kind| !weights.disabled.contains(kind) && weights.multiplier(kind) > 0.0);
        let share = weights.mutation_share;
        if isolated_mutation || (self.config.isolate.is_none() && (focused_mutation || (allowed_mutation && self.rng.chance(share)))) {
            let query = self.mutation();
            return Generated { tags: features::of(&query), query, mutation: true, partition: None };
        }
        let (query, partition) = self.read_query();
        Generated { tags: features::of(&query), query, mutation: false, partition }
    }

    // ---------------------------------------------------------------- read queries

    fn read_query(&mut self) -> (Query, Option<Expr>) {
        if self.want("next", 0.06) {
            let first = self.linear(true);
            let columns = self.returned_columns(&first);
            self.scope = columns;
            let second = self.linear_after_next();
            return (Query::Next(vec![first, second]), None);
        }
        let linear = self.linear(false);
        // The partitioning predicate is not part of the query, so its features are not the query's.
        let tags = self.tags.clone();
        let partition = if self.partitionable(&linear) { Some(self.bool_expr(1)) } else { None };
        self.tags = tags;
        let ops = [
            ("union", SetOp::Union),
            ("union_all", SetOp::UnionAll),
            ("except", SetOp::Except),
            ("intersect", SetOp::Intersect),
            ("otherwise", SetOp::Otherwise),
        ];
        for (feature, op) in ops {
            if matches!(linear.tail, Tail::Return(_)) && self.want(feature, 0.03) {
                // The other operand is the same query under another condition, so their columns agree.
                let mut other = linear.clone();
                let condition = self.bool_expr(1);
                let position = other.clauses.len();
                other.clauses.insert(position, Clause::Filter(condition));
                return (Query::Composite(Box::new(Query::Linear(linear)), op, Box::new(Query::Linear(other))), None);
            }
        }
        (Query::Linear(linear), partition)
    }

    fn partitionable(&self, linear: &Linear) -> bool {
        match &linear.tail {
            Tail::Return(ret) => {
                !ret.distinct
                    && ret.limit.is_none()
                    && ret.offset.is_none()
                    && ret.group_by.is_empty()
                    && match &ret.items {
                        ReturnItems::Items(items) => !items.iter().any(|(expr, _)| contains_aggregate(expr)),
                        ReturnItems::Star => true,
                    }
            }
            _ => false,
        }
    }

    fn returned_columns(&self, _linear: &Linear) -> Vec<Var> {
        self.returned.clone()
    }

    fn linear_after_next(&mut self) -> Linear {
        let mut clauses = Vec::new();
        if self.rng.chance(0.6) {
            let paths = vec![self.path(true)];
            let filter = self.maybe_where();
            clauses.push(Clause::Match { optional: false, mode: None, paths, filter });
        } else if !self.scope.is_empty() {
            let condition = self.bool_expr(1);
            clauses.push(Clause::Filter(condition));
        }
        let tail = self.return_tail(true);
        Linear { clauses, tail }
    }

    fn linear(&mut self, for_next: bool) -> Linear {
        let mut clauses = Vec::new();
        let matchless = self.want("matchless", 0.04);
        if matchless {
            if self.rng.chance(0.5) {
                let name = self.fresh("x");
                let value = self.int_expr(1);
                clauses.push(Clause::Let(vec![(name.clone(), value)]));
                self.scope.push(Var { name, kind: Kind::Value(Ty::Int) });
            } else {
                let name = self.fresh("x");
                let list = Expr::List((0..self.rng.range(1, 4)).map(|_| Expr::Lit(Lit::Int(self.rng.range(1, 9)))).collect());
                self.tags.insert(feature_name("list"));
                clauses.push(Clause::For { var: name.clone(), list, position: None });
                self.scope.push(Var { name, kind: Kind::Value(Ty::Int) });
            }
        } else {
            clauses.push(self.match_clause(false));
            if self.want("multi_match", 0.12) {
                clauses.push(self.match_clause(false));
            }
            if self.want("optional_match", 0.15) {
                clauses.push(self.match_clause(true));
            }
        }
        // Statements between the match and the result, in a random order.
        let mut extras: Vec<&str> = Vec::new();
        for feature in ["let", "for", "filter", "inline_call", "order_page_mid"] {
            if self.want(feature, 0.08) {
                extras.push(feature_name(feature));
            }
        }
        for feature in extras {
            match feature {
                "let" => {
                    let name = self.fresh("x");
                    let (value, ty) = self.any_scalar(1);
                    clauses.push(Clause::Let(vec![(name.clone(), value)]));
                    self.scope.push(Var { name, kind: Kind::Value(ty) });
                }
                "for" => {
                    let name = self.fresh("x");
                    let list = self.list_expr(Ty::Int);
                    let position = if self.want("for_ordinality", 0.3) {
                        Some((false, self.fresh("i")))
                    } else if self.want("for_offset", 0.2) {
                        Some((true, self.fresh("i")))
                    } else {
                        None
                    };
                    if let Some((_, position)) = &position {
                        self.scope.push(Var { name: position.clone(), kind: Kind::Value(Ty::Int) });
                    }
                    clauses.push(Clause::For { var: name.clone(), list, position });
                    self.scope.push(Var { name, kind: Kind::Value(Ty::Int) });
                }
                "filter" => {
                    let condition = self.bool_expr(2);
                    clauses.push(Clause::Filter(condition));
                }
                "inline_call" => clauses.push(self.inline_call()),
                "order_page_mid" => {
                    // Paging before RETURN keeps a deterministic prefix only under a total order, so it orders by
                    // every node identity in scope.
                    let order = self.total_order();
                    if !order.is_empty() {
                        let limit = Some(self.rng.range(1, 6) as u32);
                        clauses.push(Clause::OrderPage { order, offset: None, limit });
                    }
                }
                _ => unreachable!(),
            }
        }
        if !for_next && self.want("finish", 0.02) {
            return Linear { clauses, tail: Tail::Finish };
        }
        let tail = self.return_tail(for_next);
        Linear { clauses, tail }
    }

    fn total_order(&mut self) -> Vec<OrderItem> {
        let mut order = Vec::new();
        for var in self.scope.clone() {
            match var.kind {
                Kind::Node(_) => order.push(OrderItem { expr: Expr::Prop(var.name.clone(), "id".to_string()), desc: false, nulls: None }),
                Kind::Value(_) => order.push(OrderItem { expr: Expr::Var(var.name.clone()), desc: false, nulls: None }),
                _ => {}
            }
        }
        // Edges between the same nodes are told apart by no returned value, so paging over them is avoided.
        if self.scope.iter().any(|var| matches!(var.kind, Kind::Edge(_) | Kind::Path)) {
            return Vec::new();
        }
        order
    }

    fn inline_call(&mut self) -> Clause {
        let optional = self.want("optional_call", 0.3);
        let nodes: Vec<Var> = self.scope.iter().filter(|var| matches!(var.kind, Kind::Node(_))).cloned().collect();
        let outer = std::mem::take(&mut self.scope);
        let scope = if nodes.is_empty() || self.want("call_no_scope", 0.2) {
            if !nodes.is_empty() {
                self.scope = outer.clone();
            }
            None
        } else {
            let node = self.rng.pick(&nodes).clone();
            self.scope = vec![node.clone()];
            Some(vec![node.name])
        };
        // The body matches from an imported node and returns one new value.
        let mut clauses = Vec::new();
        let start = self.scope.iter().find(|var| matches!(var.kind, Kind::Node(_))).cloned();
        let path = match start {
            Some(var) => self.path_from(Some(var)),
            None => self.path(false),
        };
        clauses.push(Clause::Match { optional: false, mode: None, paths: vec![path], filter: None });
        let name = self.fresh("c");
        let value = if self.want("agg_count", 0.5) { Expr::Agg { name: "count".to_string(), distinct: false, arg: None } } else { self.int_expr(1) };
        let aggregated = contains_aggregate(&value);
        let body = Linear {
            clauses,
            tail: Tail::Return(Return {
                distinct: false,
                items: ReturnItems::Items(vec![(value, Some(name.clone()))]),
                group_by: vec![],
                order: vec![],
                offset: None,
                limit: None,
            }),
        };
        self.scope = outer;
        // Without an aggregate a body that matches nothing yields NULL under OPTIONAL CALL.
        let _ = aggregated;
        self.scope.push(Var { name, kind: Kind::Value(Ty::Int) });
        Clause::Call { optional, scope, body: Box::new(Query::Linear(body)) }
    }

    fn match_clause(&mut self, optional: bool) -> Clause {
        let mut paths = vec![self.path(!self.scope.is_empty())];
        if self.want("multi_path", 0.12) {
            paths.push(self.path(true));
        }
        let mode = if self.want("match_repeatable", 0.03) {
            Some(MatchMode::RepeatableElements)
        } else if self.want("match_different", 0.03) {
            Some(MatchMode::DifferentEdges)
        } else {
            None
        };
        let filter = self.maybe_where();
        Clause::Match { optional, mode, paths, filter }
    }

    fn maybe_where(&mut self) -> Option<Expr> {
        if self.config.isolate.is_some() || self.rng.chance(0.55) { Some(self.bool_expr(2)) } else { None }
    }

    /// A path that may start at a node already in scope, so patterns join.
    fn path(&mut self, may_reuse: bool) -> PathPat {
        let reuse = if may_reuse && self.rng.chance(0.5) {
            let nodes: Vec<Var> = self.scope.iter().filter(|var| matches!(var.kind, Kind::Node(_))).cloned().collect();
            if nodes.is_empty() { None } else { Some(self.rng.pick(&nodes).clone()) }
        } else {
            None
        };
        self.path_from(reuse)
    }

    fn path_from(&mut self, start: Option<Var>) -> PathPat {
        let start_label = match &start {
            Some(Var { kind: Kind::Node(label), .. }) => *label,
            _ => Some(self.rng.pick(&self.schema.nodes).label),
        };
        let mut search = None;
        let mut mode = None;
        let mut quantified_hop = false;
        let searches = [
            ("any_shortest", PathSearch::AnyShortest),
            ("all_shortest", PathSearch::AllShortest),
            ("shortest_k", PathSearch::ShortestK(2)),
            ("shortest_k_groups", PathSearch::ShortestKGroups(2)),
            ("any_k", PathSearch::Any(2)),
        ];
        for (feature, value) in searches {
            if search.is_none() && !self.in_exists && self.want(feature, 0.02) {
                search = Some(value);
                quantified_hop = true;
            }
        }
        let modes = [("path_trail", PathMode::Trail), ("path_acyclic", PathMode::Acyclic), ("path_simple", PathMode::Simple), ("path_walk", PathMode::Walk)];
        for (feature, value) in modes {
            if mode.is_none() && !self.in_exists && self.want(feature, 0.02) {
                mode = Some(value);
                quantified_hop = true;
            }
        }
        let start_node = match &start {
            Some(var) => NodePat { var: Some(var.name.clone()), ..Default::default() },
            None => self.node_pattern(start_label, true),
        };
        let hops_count = if self.want("two_hops", 0.2) {
            2
        } else if self.rng.chance(0.75) || quantified_hop {
            1
        } else {
            0
        };
        let mut hops = Vec::new();
        let mut current = start_label;
        for hop in 0..hops_count {
            let force_quantifier = quantified_hop && hop == 0;
            let (edge, next) = self.hop(current, force_quantifier, search.is_some());
            current = next;
            let node = self.node_pattern(next, hop + 1 == hops_count);
            hops.push((edge, node));
        }
        let var = if search.is_some() || (!self.in_exists && self.want("path_var", 0.05)) {
            let name = self.fresh("p");
            Some(name)
        } else {
            None
        };
        if let Some(name) = &var {
            self.scope.push(Var { name: name.clone(), kind: Kind::Path });
        }
        if let Some(var) = start {
            // A reused node is already in scope.
            let _ = var;
        }
        PathPat { var, search, mode, start: start_node, hops }
    }

    /// One edge from a node of label `from`, and the label of the node it reaches.
    fn hop(&mut self, from: Option<&'static str>, force_quantifier: bool, searched: bool) -> (EdgePat, Option<&'static str>) {
        let candidates: Vec<(usize, bool)> = self
            .schema
            .edges
            .iter()
            .enumerate()
            .flat_map(|(index, edge)| {
                let mut ways = Vec::new();
                if from.is_none() || from == Some(edge.source) {
                    ways.push((index, false));
                }
                if from.is_none() || from == Some(edge.target) {
                    ways.push((index, true));
                }
                ways
            })
            .collect();
        // An edge drawn leftwards is one whose target is this node: flipping an outgoing type would match nothing.
        let incoming: Vec<(usize, bool)> = candidates.iter().copied().filter(|(_, backwards)| *backwards).collect();
        let pool = if !incoming.is_empty() && self.want("edge_left", 0.1) { incoming } else { candidates };
        let (index, backwards) = if pool.is_empty() { (0, false) } else { *self.rng.pick(&pool) };
        let edge_type = &self.schema.edges[index];
        let next = if backwards { edge_type.source } else { edge_type.target };
        let mut dir = if edge_type.undirected {
            Dir::Undirected
        } else if backwards {
            Dir::Left
        } else {
            Dir::Right
        };
        let quantifiable = edge_type.source == edge_type.target;
        let mut quant = None;
        if quantifiable || force_quantifier {
            let quants = [("quant_fixed", Quant::Fixed(2)), ("quant_range", Quant::Range(1, 3)), ("quant_plus", Quant::Plus), ("quant_star", Quant::Star)];
            for (feature, value) in quants {
                if quant.is_none() && self.want(feature, 0.04) {
                    quant = Some(value);
                }
            }
            if force_quantifier && quant.is_none() {
                quant = Some(if searched { Quant::Plus } else { Quant::Range(1, 3) });
            }
        }
        // A quantified edge over different endpoint types cannot repeat, so it takes a type from a node type to
        // itself, or no quantifier when the schema has none.
        let mut label = Some(LabelExpr::Name(edge_type.label.to_string()));
        let mut next_label = Some(next);
        if quant.is_some() && !quantifiable {
            // A self-loop type that starts where this hop does; another would match nothing from here.
            let loops: Vec<(&'static str, &'static str)> = self
                .schema
                .edges
                .iter()
                .filter(|edge| edge.source == edge.target && !edge.undirected && from.is_none_or(|from| from == edge.source))
                .map(|edge| (edge.label, edge.source))
                .collect();
            if loops.is_empty() {
                quant = None;
            } else {
                let (edge_label, node_label) = *self.rng.pick(&loops);
                label = Some(LabelExpr::Name(edge_label.to_string()));
                next_label = Some(node_label);
                dir = Dir::Right;
            }
        }
        if !edge_type.undirected {
            if self.want("edge_any", 0.1) {
                dir = Dir::Any;
                next_label = if quant.is_some() { next_label } else { None };
            } else if self.want("edge_left_or_right", 0.03) {
                dir = Dir::LeftOrRight;
                next_label = if quant.is_some() { next_label } else { None };
            }
        } else if self.want("edge_left_or_undirected", 0.05) {
            dir = Dir::LeftOrUndirected;
        } else if self.want("edge_undirected_or_right", 0.05) {
            dir = Dir::UndirectedOrRight;
        }
        let undirected: Vec<(&'static str, &'static str)> =
            self.schema.edges.iter().filter(|edge| edge.undirected).map(|edge| (edge.label, edge.target)).collect();
        if self.config.undirected && quant.is_none() && !undirected.is_empty() && self.want("edge_undirected", 0.06) {
            let (edge_label, node_label) = *self.rng.pick(&undirected);
            dir = Dir::Undirected;
            label = Some(LabelExpr::Name(edge_label.to_string()));
            next_label = Some(node_label);
        }
        if quant.is_none() && self.want("unlabeled_edge", 0.12) {
            label = None;
            next_label = None;
        } else if quant.is_none() && self.want("label_or", 0.05) {
            let other = self.rng.pick(&self.schema.edges).label.to_string();
            label = Some(LabelExpr::Or(Box::new(label.unwrap_or(LabelExpr::Wildcard)), Box::new(LabelExpr::Name(other))));
            next_label = None;
        } else if quant.is_none() && self.want("label_not", 0.03) {
            label = Some(LabelExpr::Not(Box::new(label.unwrap_or(LabelExpr::Wildcard))));
            next_label = None;
        }
        // Paths that reach the start node again stay possible: next_label None means any node.
        let _ = from;
        let var = if quant.is_none() && (self.config.isolate.is_none() && self.rng.chance(0.25) || self.want("edge_var", 0.2)) {
            self.tags.insert(feature_name("edge_var"));
            let name = self.fresh("e");
            let edge_label = match &label {
                Some(LabelExpr::Name(name)) => self.schema.edges.iter().find(|edge| edge.label == name).map(|edge| edge.label),
                _ => None,
            };
            self.scope.push(Var { name: name.clone(), kind: Kind::Edge(edge_label) });
            Some(name)
        } else {
            None
        };
        let mut props = Vec::new();
        let mut filter = None;
        if var.is_some() && self.want("element_where", 0.04) {
            filter = Some(self.bool_expr(1));
        }
        if quant.is_none()
            && self.want("property_map", 0.03)
            && let Some(LabelExpr::Name(name)) = &label
            && let Some(prop) = self.schema.edge(name).props.first()
            && self.literal_allowed(prop.ty)
        {
            // A value of the property's type: a small integer, as the fixture's, or a random one of another type.
            let value = match prop.ty {
                Ty::Int => Lit::Int(self.rng.range(1, 30)),
                ty => ty.random(&mut self.rng),
            };
            props.push((prop.name.to_string(), Expr::Lit(value)));
        }
        (EdgePat { var, label, dir, quant, props, filter }, next_label)
    }

    fn node_pattern(&mut self, label: Option<&'static str>, named: bool) -> NodePat {
        let mut label_expr = label.map(|label| LabelExpr::Name(label.to_string()));
        let mut known = label;
        if self.want("unlabeled_node", 0.12) {
            label_expr = None;
            known = None;
        } else if self.want("label_or", 0.05) {
            let other = self.rng.pick(&self.schema.nodes).label;
            label_expr = Some(LabelExpr::Or(Box::new(label_expr.unwrap_or(LabelExpr::Wildcard)), Box::new(LabelExpr::Name(other.to_string()))));
            known = None;
        } else if self.want("label_not", 0.03) {
            // Excluding the label the path needs here would match nothing, so another one is excluded.
            let others: Vec<&'static str> = self.schema.nodes.iter().map(|node| node.label).filter(|other| Some(*other) != label).collect();
            let excluded = if others.is_empty() { self.rng.pick(&self.schema.nodes).label } else { *self.rng.pick(&others) };
            label_expr = Some(LabelExpr::Not(Box::new(LabelExpr::Name(excluded.to_string()))));
            known = None;
        } else if let Some(name) = label
            && self.want("label_and", 0.02)
        {
            // Only over a label the path allows here: another one would match nothing.
            label_expr = Some(LabelExpr::And(Box::new(LabelExpr::Name(name.to_string())), Box::new(LabelExpr::Name(name.to_string()))));
        } else if self.want("label_wildcard", 0.02) {
            label_expr = Some(LabelExpr::Wildcard);
            known = None;
        }
        let var = if named || self.rng.chance(0.6) {
            let name = self.fresh("n");
            self.scope.push(Var { name: name.clone(), kind: Kind::Node(known) });
            Some(name)
        } else {
            None
        };
        let mut props = Vec::new();
        if self.want("property_map", 0.04) {
            // An id the label's nodes have, or one most labels have.
            let most = known.map(|label| self.schema.node(label).count).unwrap_or_else(|| self.schema.nodes.iter().map(|node| node.count).min().unwrap_or(1));
            props.push(("id".to_string(), Expr::Lit(Lit::Int(self.rng.range(1, most.max(1))))));
        }
        let filter = if var.is_some() && self.want("element_where", 0.05) { Some(self.bool_expr(1)) } else { None };
        NodePat { var, label: label_expr, props, filter }
    }

    // ---------------------------------------------------------------- results

    fn return_tail(&mut self, for_next: bool) -> Tail {
        if !for_next && self.want("return_star", 0.03) {
            return Tail::Return(Return { distinct: false, items: ReturnItems::Star, group_by: vec![], order: vec![], offset: None, limit: None });
        }
        let weights = &self.config.weights;
        let aggregates: Vec<&'static str> =
            AGGREGATES.iter().copied().filter(|feature| !weights.disabled.contains(feature) && weights.multiplier(feature) > 0.0).collect();
        let weight = aggregates.iter().map(|feature| weights.multiplier(feature)).sum::<f64>() / AGGREGATES.len() as f64;
        let aggregate = match self.config.isolate.as_deref() {
            Some(target) => target.starts_with("agg_") || target == "group_by",
            None => AGGREGATES.iter().any(|feature| weights.focus.contains(feature)) || (!aggregates.is_empty() && self.rng.chance((0.25 * weight).min(1.0))),
        };
        let count = self.rng.range(1, 3) as usize;
        let mut items = Vec::new();
        let mut group_keys = Vec::new();
        self.returned.clear();
        for index in 0..count {
            let alias = format!("c{index}");
            let (expr, kind) = if aggregate && index == count - 1 {
                self.aggregate()
            } else {
                let (expr, ty) = self.any_scalar(1);
                (expr, Kind::Value(ty))
            };
            self.returned.push(Var { name: alias.clone(), kind });
            if !contains_aggregate(&expr) {
                group_keys.push(alias.clone());
            }
            items.push((expr, Some(alias)));
        }
        if !items.iter().any(|(expr, _)| contains_aggregate(expr)) && for_next {
            // NEXT reads named values.
        }
        let distinct = self.want("return_distinct", 0.08);
        let has_aggregate = items.iter().any(|(expr, _)| contains_aggregate(expr));
        let group_by = if has_aggregate && !group_keys.is_empty() && self.want("group_by", 0.3) { group_keys } else { vec![] };
        let mut order = Vec::new();
        let mut limit = None;
        let mut offset = None;
        let ordered = self.want("order_by", 0.15);
        let paged_limit = self.want("limit", 0.08);
        let paged_offset = self.want("offset", 0.05);
        if ordered || paged_limit || paged_offset {
            // Order by every returned column, so a page of the result is the same in both engines.
            let desc = self.want("order_desc", 0.3);
            for index in 0..items.len() {
                order.push(OrderItem { expr: Expr::Var(format!("c{index}")), desc, nulls: None });
            }
            self.tags.insert(feature_name("order_by"));
            if paged_limit {
                limit = Some(self.rng.range(0, 5) as u32);
            }
            if paged_offset {
                offset = Some(self.rng.range(0, 3) as u32);
            }
        }
        Tail::Return(Return { distinct, items: ReturnItems::Items(items), group_by, order, offset, limit })
    }

    fn aggregate(&mut self) -> (Expr, Kind) {
        let feature = match self.config.isolate.clone() {
            Some(target) if AGGREGATES.contains(&target.as_str()) => feature_name(&target),
            _ => self.weighted_choice(AGGREGATES),
        };
        self.tags.insert(feature);
        let count = |arg: Option<Expr>, distinct: bool| Expr::Agg { name: "count".to_string(), distinct, arg: arg.map(Box::new) };
        match feature {
            "agg_count" => (count(None, false), Kind::Value(Ty::Int)),
            "agg_count_expr" => (count(Some(self.any_scalar(1).0), false), Kind::Value(Ty::Int)),
            "agg_count_distinct" => (count(Some(self.any_scalar(1).0), true), Kind::Value(Ty::Int)),
            "agg_sum" => (Expr::Agg { name: "sum".to_string(), distinct: false, arg: Some(Box::new(self.int_expr(1))) }, Kind::Value(Ty::Int)),
            "agg_avg" => (Expr::Agg { name: "avg".to_string(), distinct: false, arg: Some(Box::new(self.int_expr(1))) }, Kind::Value(Ty::Float)),
            "agg_min_max" => {
                let name = if self.rng.chance(0.5) { "min" } else { "max" };
                let (arg, ty) = self.any_scalar(1);
                (Expr::Agg { name: name.to_string(), distinct: false, arg: Some(Box::new(arg)) }, Kind::Value(ty))
            }
            _ => (Expr::Agg { name: "collect_list".to_string(), distinct: false, arg: Some(Box::new(self.int_expr(1))) }, Kind::List(Ty::Int)),
        }
    }

    // ---------------------------------------------------------------- expressions

    fn vars_where(&self, test: impl Fn(&Kind) -> bool) -> Vec<Var> {
        self.scope.iter().filter(|var| test(&var.kind)).cloned().collect()
    }

    /// A property of an element in scope with the given type, or None.
    fn property(&mut self, ty: Ty) -> Option<Expr> {
        let elements = self.vars_where(|kind| matches!(kind, Kind::Node(_) | Kind::Edge(_)));
        if elements.is_empty() {
            return None;
        }
        let var = self.rng.pick(&elements).clone();
        let props: Vec<&'static str> = match var.kind {
            Kind::Node(Some(label)) => self.schema.node(label).props.iter().filter(|prop| prop.ty == ty).map(|prop| prop.name).collect(),
            Kind::Edge(Some(label)) => self.schema.edge(label).props.iter().filter(|prop| prop.ty == ty).map(|prop| prop.name).collect(),
            Kind::Node(None) => self.schema.nodes.iter().flat_map(|node| node.props.iter()).filter(|prop| prop.ty == ty).map(|prop| prop.name).collect(),
            Kind::Edge(None) => self.schema.edges.iter().flat_map(|edge| edge.props.iter()).filter(|prop| prop.ty == ty).map(|prop| prop.name).collect(),
            _ => vec![],
        };
        if props.is_empty() {
            return None;
        }
        let name = *self.rng.pick(&props);
        Some(Expr::Prop(var.name, name.to_string()))
    }

    fn value_var(&mut self, ty: Ty) -> Option<Expr> {
        let values = self.vars_where(|kind| matches!(kind, Kind::Value(value) if *value == ty));
        if values.is_empty() { None } else { Some(Expr::Var(self.rng.pick(&values).name.clone())) }
    }

    /// A value type: the basic ones most often, and the schema's other property types too.
    fn pick_type(&mut self) -> Ty {
        let mut types = vec![Ty::Int, Ty::Int, Ty::Str, Ty::Float, Ty::Bool];
        types.extend(self.schema.types().into_iter().filter(|ty| !matches!(ty, Ty::Int | Ty::Str | Ty::Float | Ty::Bool)));
        *self.rng.pick(&types)
    }

    fn expr_of(&mut self, ty: Ty, depth: u32) -> Expr {
        match ty {
            Ty::Int => self.int_expr(depth),
            Ty::Str => self.str_expr(depth),
            Ty::Float => self.float_expr(depth),
            Ty::Bool => self.bool_expr(depth),
            Ty::Decimal => self.decimal_expr(depth),
            Ty::Date | Ty::LocalDateTime | Ty::ZonedDateTime => self.temporal_expr(ty, depth),
        }
    }

    fn any_scalar(&mut self, depth: u32) -> (Expr, Ty) {
        let ty = self.pick_type();
        (self.expr_of(ty, depth), ty)
    }

    /// Exact arithmetic on decimals; engines without a decimal type compute it approximately, within the
    /// seven digits values are compared to.
    fn decimal_expr(&mut self, depth: u32) -> Expr {
        if depth > 0 && self.want("arith", 0.1) {
            let op = *self.rng.pick(&[BinOp::Add, BinOp::Sub]);
            return Expr::Bin(op, Box::new(self.decimal_expr(depth - 1)), Box::new(self.decimal_expr(0)));
        }
        self.leaf(Ty::Decimal)
    }

    /// A date or datetime: a property, a literal, or one of them through COALESCE.
    fn temporal_expr(&mut self, ty: Ty, depth: u32) -> Expr {
        if depth > 0 && self.want("coalesce", 0.03) {
            return Expr::Func("coalesce".to_string(), vec![self.temporal_expr(ty, depth - 1), self.leaf(ty)]);
        }
        self.leaf(ty)
    }

    fn leaf(&mut self, ty: Ty) -> Expr {
        if self.rng.chance(0.7)
            && let Some(expr) = if self.rng.chance(0.75) { self.property(ty) } else { self.value_var(ty) }
        {
            return expr;
        }
        match ty {
            Ty::Int => Expr::Lit(Lit::Int(self.rng.range(0, 40))),
            Ty::Float => {
                self.tags.insert(feature_name("float_literal"));
                Expr::Lit(Lit::Float(self.rng.range(0, 80) as f64 / 8.0))
            }
            Ty::Str => {
                self.tags.insert(feature_name("string_literal"));
                Expr::Lit(Lit::Str(self.rng.pick(&["Ada", "Bob", "v1", "City2", "r0", "", "a b"]).to_string()))
            }
            Ty::Bool => Expr::Lit(Lit::Bool(self.rng.chance(0.5))),
            // Declined, a temporal value is a temporal property in scope, or NULL when the scope has none.
            ty if !self.literal_allowed(ty) => (0..4).find_map(|_| self.property(ty)).or_else(|| self.value_var(ty)).unwrap_or(Expr::Lit(Lit::Null)),
            other => Expr::Lit(other.random(&mut self.rng)),
        }
    }

    /// Whether a literal of this type may be written here. A temporal literal is a feature like any other, so
    /// isolation, weights, and `disable` govern it; engines without temporal literals would otherwise reject
    /// most queries.
    fn literal_allowed(&mut self, ty: Ty) -> bool {
        !ty.temporal() || self.want("temporal_literal", 0.25)
    }

    fn int_expr(&mut self, depth: u32) -> Expr {
        if depth == 0 {
            return self.leaf(Ty::Int);
        }
        let options = ["arith", "neg", "case", "coalesce", "nullif", "char_length", "numeric_fn", "mod", "size", "path_length", "simple_case"];
        for feature in options {
            let probability = if feature == "arith" { 0.2 } else { 0.03 };
            if self.want(feature, probability) {
                return match feature {
                    "arith" => {
                        let op = *self.rng.pick(&[BinOp::Add, BinOp::Sub, BinOp::Mul]);
                        Expr::Bin(op, Box::new(self.int_expr(depth - 1)), Box::new(self.int_expr(depth - 1)))
                    }
                    "neg" => Expr::Neg(Box::new(self.int_expr(depth - 1))),
                    "case" => Expr::Case(vec![(self.bool_expr(depth - 1), self.int_expr(depth - 1))], Some(Box::new(self.int_expr(depth - 1)))),
                    "simple_case" => Expr::SimpleCase(
                        Box::new(self.int_expr(depth - 1)),
                        vec![(Expr::Lit(Lit::Int(self.rng.range(0, 5))), self.int_expr(depth - 1))],
                        Some(Box::new(self.int_expr(depth - 1))),
                    ),
                    "coalesce" => Expr::Func("coalesce".to_string(), vec![self.int_expr(depth - 1), self.int_expr(depth - 1)]),
                    "nullif" => Expr::Func("nullif".to_string(), vec![self.int_expr(depth - 1), self.int_expr(depth - 1)]),
                    "char_length" => Expr::Func("char_length".to_string(), vec![self.str_expr(depth - 1)]),
                    "numeric_fn" => Expr::Func("abs".to_string(), vec![self.int_expr(depth - 1)]),
                    "mod" => Expr::Func("mod".to_string(), vec![self.int_expr(depth - 1), Expr::Lit(Lit::Int(self.rng.range(1, 7)))]),
                    "size" => Expr::Func("size".to_string(), vec![self.list_expr(Ty::Int)]),
                    "path_length" => {
                        let paths = self.vars_where(|kind| matches!(kind, Kind::Path));
                        if paths.is_empty() {
                            self.tags.remove("path_length");
                            continue;
                        }
                        Expr::Func("path_length".to_string(), vec![Expr::Var(self.rng.pick(&paths).name.clone())])
                    }
                    _ => unreachable!(),
                };
            }
        }
        self.leaf(Ty::Int)
    }

    fn float_expr(&mut self, depth: u32) -> Expr {
        if depth > 0 {
            if self.want("numeric_fn", 0.05) {
                let name = *self.rng.pick(&["ceil", "floor", "sqrt"]);
                let arg = if name == "sqrt" { Expr::Func("abs".to_string(), vec![self.float_expr(depth - 1)]) } else { self.float_expr(depth - 1) };
                return Expr::Func(name.to_string(), vec![arg]);
            }
            if self.want("power", 0.02) {
                return Expr::Func("power".to_string(), vec![self.float_expr(depth - 1), Expr::Lit(Lit::Int(2))]);
            }
            if self.want("trig", 0.02) {
                return Expr::Func(self.rng.pick(&["sin", "cos"]).to_string(), vec![self.float_expr(depth - 1)]);
            }
            if self.want("log_fn", 0.02) {
                return Expr::Func("exp".to_string(), vec![Expr::Func("sin".to_string(), vec![self.float_expr(depth - 1)])]);
            }
            if self.want("div", 0.03) {
                return Expr::Bin(BinOp::Div, Box::new(self.float_expr(depth - 1)), Box::new(Expr::Lit(Lit::Float(2.0))));
            }
            if self.want("arith", 0.1) {
                let op = *self.rng.pick(&[BinOp::Add, BinOp::Sub, BinOp::Mul]);
                return Expr::Bin(op, Box::new(self.float_expr(depth - 1)), Box::new(self.float_expr(depth - 1)));
            }
        }
        self.leaf(Ty::Float)
    }

    fn str_expr(&mut self, depth: u32) -> Expr {
        if depth > 0 {
            if self.want("string_fn", 0.06) {
                let name = *self.rng.pick(&["upper", "lower", "trim", "left", "right"]);
                let arg = self.str_expr(depth - 1);
                return if name == "left" || name == "right" {
                    Expr::Func(name.to_string(), vec![arg, Expr::Lit(Lit::Int(self.rng.range(0, 3)))])
                } else {
                    Expr::Func(name.to_string(), vec![arg])
                };
            }
            if self.want("concat", 0.05) {
                return Expr::Bin(BinOp::Concat, Box::new(self.str_expr(depth - 1)), Box::new(self.str_expr(depth - 1)));
            }
            if self.want("cast", 0.03) {
                return Expr::Cast(Box::new(self.int_expr(depth - 1)), "STRING".to_string());
            }
            if self.want("coalesce", 0.03) {
                return Expr::Func("coalesce".to_string(), vec![self.str_expr(depth - 1), self.str_expr(depth - 1)]);
            }
        }
        self.leaf(Ty::Str)
    }

    fn list_expr(&mut self, ty: Ty) -> Expr {
        self.tags.insert(feature_name("list"));
        // An empty list is a case worth having, but a FOR over one binds nothing, so it is rare.
        let count = if self.rng.chance(0.1) { 0 } else { self.rng.range(1, 3) };
        Expr::List((0..count).map(|_| self.leaf(ty)).collect())
    }

    fn bool_expr(&mut self, depth: u32) -> Expr {
        if depth > 0 {
            let options = [
                ("or", 0.12),
                ("not", 0.06),
                ("xor", 0.02),
                ("is_null", 0.06),
                ("is_typed", 0.03),
                ("is_normalized", 0.02),
                ("is_labeled", 0.03),
                ("is_directed", 0.02),
                ("source_of", 0.02),
                ("all_different", 0.02),
                ("same", 0.02),
                ("property_exists", 0.02),
                ("exists_pattern", 0.03),
                ("exists_match", 0.02),
                ("element_id", 0.02),
                ("case", 0.02),
            ];
            for (feature, probability) in options {
                if !self.want(feature, probability) {
                    continue;
                }
                let nodes = self.vars_where(|kind| matches!(kind, Kind::Node(_)));
                let edges = self.vars_where(|kind| matches!(kind, Kind::Edge(_)));
                let built = match feature {
                    "or" => Some(Expr::Bin(BinOp::Or, Box::new(self.bool_expr(depth - 1)), Box::new(self.bool_expr(depth - 1)))),
                    "not" => Some(Expr::Not(Box::new(self.bool_expr(depth - 1)))),
                    "xor" => Some(Expr::Bin(BinOp::Xor, Box::new(self.bool_expr(depth - 1)), Box::new(self.bool_expr(depth - 1)))),
                    "is_null" => {
                        let (value, _) = self.any_scalar(0);
                        Some(Expr::IsNull(Box::new(value), self.rng.chance(0.5)))
                    }
                    "is_typed" => {
                        let (value, _) = self.any_scalar(0);
                        let ty = *self.rng.pick(&["INTEGER", "FLOAT", "STRING", "BOOLEAN", "INT64", "DATE"]);
                        Some(Expr::Typed(Box::new(value), ty.to_string(), self.rng.chance(0.3)))
                    }
                    "is_normalized" => Some(Expr::Normalized(Box::new(self.str_expr(0)), self.rng.chance(0.3))),
                    "is_labeled" => nodes.first().map(|node| {
                        // Half the time the label the node was matched with, so the test is not always false.
                        let own = match node.kind {
                            Kind::Node(Some(label)) if self.rng.chance(0.5) => label,
                            _ => self.rng.pick(&self.schema.nodes).label,
                        };
                        Expr::Labeled(node.name.clone(), LabelExpr::Name(own.to_string()), self.rng.chance(0.3))
                    }),
                    "is_directed" => edges.first().map(|edge| Expr::Directed(edge.name.clone(), self.rng.chance(0.3))),
                    "source_of" => match (nodes.first(), edges.first()) {
                        (Some(node), Some(edge)) => Some(if self.rng.chance(0.5) {
                            Expr::SourceOf(node.name.clone(), edge.name.clone(), false)
                        } else {
                            Expr::DestinationOf(node.name.clone(), edge.name.clone(), false)
                        }),
                        _ => None,
                    },
                    "all_different" | "same" if nodes.len() >= 2 => {
                        let names = nodes.iter().take(3).map(|node| node.name.clone()).collect();
                        Some(if feature == "same" { Expr::Same(names) } else { Expr::AllDifferent(names) })
                    }
                    "property_exists" => nodes.first().map(|node| {
                        let names: Vec<&'static str> = self.schema.nodes.iter().flat_map(|node| node.props.iter().map(|prop| prop.name)).collect();
                        Expr::PropertyExists(node.name.clone(), self.rng.pick(&names).to_string())
                    }),
                    "exists_pattern" | "exists_match" if !nodes.is_empty() => {
                        let node = self.rng.pick(&nodes).clone();
                        let saved = self.scope.clone();
                        self.in_exists = true;
                        let path = self.path_from(Some(node));
                        self.in_exists = false;
                        let condition = if feature == "exists_match" { Some(self.bool_expr(0)) } else { None };
                        self.scope = saved;
                        Some(Expr::Exists(Box::new(if feature == "exists_match" {
                            ExistsBody::Match(vec![path], condition)
                        } else {
                            ExistsBody::Pattern(vec![path], None)
                        })))
                    }
                    "element_id" if nodes.len() >= 2 => Some(Expr::Bin(
                        BinOp::Eq,
                        Box::new(Expr::Func("element_id".to_string(), vec![Expr::Var(nodes[0].name.clone())])),
                        Box::new(Expr::Func("element_id".to_string(), vec![Expr::Var(nodes[1].name.clone())])),
                    )),
                    "case" => Some(Expr::Case(vec![(self.bool_expr(depth - 1), self.bool_expr(0))], Some(Box::new(self.bool_expr(0))))),
                    _ => None,
                };
                match built {
                    Some(expr) => return expr,
                    None => {
                        self.tags.remove(feature);
                    }
                }
            }
        }
        // The base: a comparison, or two joined by AND.
        let comparison = self.comparison(depth);
        if depth > 0 && self.rng.chance(0.25) {
            let other = self.comparison(depth - 1);
            return Expr::Bin(BinOp::And, Box::new(comparison), Box::new(other));
        }
        comparison
    }

    /// A comparison that mostly splits the rows rather than keeping all or none: it reads a property of a bound
    /// element and compares it with a value the data holds for that property. Comparisons of two literals, of
    /// an expression with itself, and with NULL still occur, but rarely, since they bind all rows or none.
    fn comparison(&mut self, depth: u32) -> Expr {
        let op = *self.rng.pick(&[BinOp::Eq, BinOp::Ne, BinOp::Lt, BinOp::Le, BinOp::Gt, BinOp::Ge]);
        let mut ty = self.pick_type();
        // Without a value of the type in scope, both sides would be literals (or NULL, for a temporal type).
        if !self.scope_has(ty) && self.rng.chance(0.9) {
            ty = [Ty::Int, Ty::Str, Ty::Float, Ty::Bool].into_iter().find(|ty| self.scope_has(*ty)).unwrap_or(ty);
        }
        let child = depth.saturating_sub(1);
        let reads_data = self.rng.chance(0.95);
        let left = if reads_data { (0..4).find_map(|_| self.property(ty)) } else { None };
        let left = left.unwrap_or_else(|| if ty == Ty::Bool { self.leaf(Ty::Bool) } else { self.expr_of(ty, child) });
        let from_data = match &left {
            // Literals of these types almost never equal a value in the data.
            Expr::Prop(var, prop) if self.rng.chance(0.9) || matches!(ty, Ty::Float | Ty::Decimal) || ty.temporal() => {
                self.data_value(var, prop).map(Expr::Lit)
            }
            _ => None,
        };
        let right = from_data.unwrap_or_else(|| {
            // Anything but the left side again, unless nothing else comes up.
            let mut right = if ty == Ty::Bool { Expr::Lit(Lit::Bool(self.rng.chance(0.5))) } else { self.expr_of(ty, 0) };
            for _ in 0..3 {
                if format!("{right:?}") != format!("{left:?}") || self.rng.chance(0.05) {
                    break;
                }
                right = self.expr_of(ty, 0);
            }
            right
        });
        let op = if ty == Ty::Bool { *self.rng.pick(&[BinOp::Eq, BinOp::Ne]) } else { op };
        Expr::Bin(op, Box::new(left), Box::new(right))
    }

    /// Whether some element or value in scope has a value of this type to read.
    fn scope_has(&self, ty: Ty) -> bool {
        let has = |props: &[crate::schema::Prop]| props.iter().any(|prop| prop.ty == ty);
        self.scope.iter().any(|var| match &var.kind {
            Kind::Node(Some(label)) => has(&self.schema.node(label).props),
            Kind::Edge(Some(label)) => has(&self.schema.edge(label).props),
            Kind::Node(None) => self.schema.nodes.iter().any(|node| has(&node.props)),
            Kind::Edge(None) => self.schema.edges.iter().any(|edge| has(&edge.props)),
            Kind::Value(value) => *value == ty,
            _ => false,
        })
    }

    /// A value the fixture holds for `var.prop`, whatever label `var` matched.
    fn data_value(&mut self, var: &str, prop: &str) -> Option<Lit> {
        let kind = self.scope.iter().find(|candidate| candidate.name == var)?.kind.clone();
        let values: Vec<&Lit> = self
            .schema
            .values
            .iter()
            .filter(|((label, name), _)| {
                *name == prop
                    && match &kind {
                        Kind::Node(Some(bound)) | Kind::Edge(Some(bound)) => bound == label,
                        Kind::Node(None) => self.schema.nodes.iter().any(|node| node.label == *label),
                        Kind::Edge(None) => self.schema.edges.iter().any(|edge| edge.label == *label),
                        _ => false,
                    }
            })
            .flat_map(|(_, values)| values.iter())
            .collect();
        if values.is_empty() { None } else { Some((*self.rng.pick(&values)).clone()) }
    }

    // ---------------------------------------------------------------- data modification

    fn new_id(&mut self) -> i64 {
        self.next_id += 1;
        self.next_id
    }

    fn insert_node_pattern(&mut self, label: &'static str, named: bool) -> NodePat {
        let mut props = vec![("id".to_string(), Expr::Lit(Lit::Int(self.new_id())))];
        for prop in &self.schema.node(label).props {
            if prop.required || self.rng.chance(0.5) || !self.literal_allowed(prop.ty) {
                continue;
            }
            let value = match prop.ty {
                Ty::Str => Expr::Lit(Lit::Str(format!("new{}", self.rng.range(0, 9)))),
                ty => Expr::Lit(ty.random(&mut self.rng)),
            };
            props.push((prop.name.to_string(), value));
        }
        let var = if named { Some(self.fresh("m")) } else { None };
        NodePat { var, label: Some(LabelExpr::Name(label.to_string())), props, filter: None }
    }

    fn mutation_kind(&mut self, kinds: &[&'static str]) -> &'static str {
        self.weighted_choice(kinds)
    }

    /// One of several alternatives: a focused one if any, else one drawn by weight, never a disabled one.
    fn weighted_choice(&mut self, kinds: &[&'static str]) -> &'static str {
        let weights = &self.config.weights;
        let focused: Vec<&'static str> = kinds.iter().copied().filter(|kind| weights.focus.contains(kind)).collect();
        if !focused.is_empty() {
            return self.rng.pick(&focused);
        }
        let weighted: Vec<(&'static str, f64)> = kinds
            .iter()
            .copied()
            .filter(|kind| !weights.disabled.contains(kind))
            .map(|kind| (kind, weights.multiplier(kind)))
            .filter(|(_, weight)| *weight > 0.0)
            .collect();
        let total: f64 = weighted.iter().map(|(_, weight)| weight).sum();
        let mut draw = (self.rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64 * total;
        for (kind, weight) in &weighted {
            if draw < *weight {
                return kind;
            }
            draw -= weight;
        }
        weighted.last().map(|(kind, _)| *kind).unwrap_or(kinds[0])
    }

    fn mutation(&mut self) -> Query {
        let kinds = [
            "insert_node",
            "insert_path",
            "match_insert",
            "multi_insert",
            "set_prop",
            "remove_prop",
            "delete_edge",
            "detach_delete",
            "delete_attached",
            "for_insert",
            "next_set",
        ];
        let kind = match &self.config.isolate {
            Some(target) => feature_name(target),
            None => self.mutation_kind(&kinds),
        };
        self.tags.insert(kind);
        let linear = |clauses: Vec<Clause>| Query::Linear(Linear { clauses, tail: Tail::None });
        match kind {
            "insert_node" => {
                let label = self.rng.pick(&self.schema.nodes).label;
                let node = self.insert_node_pattern(label, false);
                linear(vec![Clause::Insert(vec![PathPat { var: None, search: None, mode: None, start: node, hops: vec![] }])])
            }
            "insert_path" | "multi_insert" => {
                let edge = self.rng.pick(&self.schema.edges);
                let (label, source, target, undirected) = (edge.label, edge.source, edge.target, edge.undirected);
                let start = self.insert_node_pattern(source, kind == "multi_insert");
                let end = self.insert_node_pattern(target, kind == "multi_insert");
                let edge_pat = EdgePat {
                    var: None,
                    label: Some(LabelExpr::Name(label.to_string())),
                    dir: if undirected { Dir::Undirected } else { Dir::Right },
                    quant: None,
                    props: vec![],
                    filter: None,
                };
                if kind == "multi_insert" {
                    let start_var = start.var.clone();
                    let end_var = end.var.clone();
                    let first = Clause::Insert(vec![PathPat { var: None, search: None, mode: None, start, hops: vec![] }]);
                    let second = Clause::Insert(vec![PathPat { var: None, search: None, mode: None, start: end, hops: vec![] }]);
                    let third = Clause::Insert(vec![PathPat {
                        var: None,
                        search: None,
                        mode: None,
                        start: NodePat { var: start_var, ..Default::default() },
                        hops: vec![(edge_pat, NodePat { var: end_var, ..Default::default() })],
                    }]);
                    return linear(vec![first, second, third]);
                }
                linear(vec![Clause::Insert(vec![PathPat { var: None, search: None, mode: None, start, hops: vec![(edge_pat, end)] }])])
            }
            "match_insert" => {
                let edge = self.rng.pick(&self.schema.edges);
                let (label, source, target, undirected) = (edge.label, edge.source, edge.target, edge.undirected);
                let first = NodePat { var: Some("a".to_string()), label: Some(LabelExpr::Name(source.to_string())), props: vec![], filter: None };
                let second = NodePat { var: Some("b".to_string()), label: Some(LabelExpr::Name(target.to_string())), props: vec![], filter: None };
                self.scope = vec![Var { name: "a".to_string(), kind: Kind::Node(Some(source)) }, Var { name: "b".to_string(), kind: Kind::Node(Some(target)) }];
                let condition = Expr::Bin(
                    BinOp::And,
                    Box::new(Expr::Bin(
                        BinOp::Le,
                        Box::new(Expr::Prop("a".to_string(), "id".to_string())),
                        Box::new(Expr::Lit(Lit::Int(self.rng.range(1, 3)))),
                    )),
                    Box::new(Expr::Bin(
                        BinOp::Eq,
                        Box::new(Expr::Prop("b".to_string(), "id".to_string())),
                        Box::new(Expr::Lit(Lit::Int(self.rng.range(1, 3)))),
                    )),
                );
                let props = match self.schema.edge(label).props.first() {
                    Some(prop) if prop.ty == Ty::Int => vec![(prop.name.to_string(), self.int_expr(1))],
                    _ => vec![],
                };
                let edge_pat = EdgePat {
                    var: None,
                    label: Some(LabelExpr::Name(label.to_string())),
                    dir: if undirected { Dir::Undirected } else { Dir::Right },
                    quant: None,
                    props,
                    filter: None,
                };
                linear(vec![
                    Clause::Match {
                        optional: false,
                        mode: None,
                        paths: vec![
                            PathPat { var: None, search: None, mode: None, start: first, hops: vec![] },
                            PathPat { var: None, search: None, mode: None, start: second, hops: vec![] },
                        ],
                        filter: Some(condition),
                    },
                    Clause::Insert(vec![PathPat {
                        var: None,
                        search: None,
                        mode: None,
                        start: NodePat { var: Some("a".to_string()), ..Default::default() },
                        hops: vec![(edge_pat, NodePat { var: Some("b".to_string()), ..Default::default() })],
                    }]),
                ])
            }
            "set_prop" | "remove_prop" => {
                // One node per row: Neo4j applies SET row by row, GQL from one snapshot, so a node matched by several
                // rows would differ.
                let label = self.rng.pick(&self.schema.nodes).label;
                let node = self.node_pattern(Some(label), true);
                let filter = self.maybe_where();
                let mut clauses = vec![Clause::Match {
                    optional: false,
                    mode: None,
                    paths: vec![PathPat { var: None, search: None, mode: None, start: node, hops: vec![] }],
                    filter,
                }];
                let nodes = self.vars_where(|kind| matches!(kind, Kind::Node(Some(_))));
                let Some(node) = nodes.first().cloned() else {
                    return linear(clauses);
                };
                let Kind::Node(Some(label)) = node.kind else { unreachable!() };
                let props: Vec<(&'static str, Ty)> =
                    self.schema.node(label).props.iter().filter(|prop| !prop.required).map(|prop| (prop.name, prop.ty)).collect();
                let (prop, ty) = *self.rng.pick(&props);
                if kind == "remove_prop" {
                    clauses.push(Clause::Remove(vec![RemoveItem::Prop(node.name.clone(), prop.to_string())]));
                } else {
                    let value = match ty {
                        Ty::Bool => self.bool_expr(0),
                        other => self.expr_of(other, 1),
                    };
                    clauses.push(Clause::Set(vec![SetItem::Prop(node.name.clone(), prop.to_string(), value)]));
                }
                linear(clauses)
            }
            "delete_edge" => {
                let edge = self.rng.pick(&self.schema.edges);
                let (label, source, target) = (edge.label, edge.source, edge.target);
                self.scope = vec![Var { name: "a".to_string(), kind: Kind::Node(Some(source)) }, Var { name: "e".to_string(), kind: Kind::Edge(Some(label)) }];
                let condition = self.bool_expr(1);
                let path = PathPat {
                    var: None,
                    search: None,
                    mode: None,
                    start: NodePat { var: Some("a".to_string()), label: Some(LabelExpr::Name(source.to_string())), ..Default::default() },
                    hops: vec![(
                        EdgePat {
                            var: Some("e".to_string()),
                            label: Some(LabelExpr::Name(label.to_string())),
                            dir: Dir::Right,
                            quant: None,
                            props: vec![],
                            filter: None,
                        },
                        NodePat { var: None, label: Some(LabelExpr::Name(target.to_string())), ..Default::default() },
                    )],
                };
                linear(vec![
                    Clause::Match { optional: false, mode: None, paths: vec![path], filter: Some(condition) },
                    Clause::Delete { detach: false, vars: vec!["e".to_string()] },
                ])
            }
            "detach_delete" | "delete_attached" => {
                let label = self.rng.pick(&self.schema.nodes).label;
                let path = PathPat {
                    var: None,
                    search: None,
                    mode: None,
                    start: NodePat { var: Some("d".to_string()), label: Some(LabelExpr::Name(label.to_string())), ..Default::default() },
                    hops: vec![],
                };
                let condition =
                    Expr::Bin(BinOp::Eq, Box::new(Expr::Prop("d".to_string(), "id".to_string())), Box::new(Expr::Lit(Lit::Int(self.rng.range(1, 4)))));
                linear(vec![
                    Clause::Match { optional: false, mode: None, paths: vec![path], filter: Some(condition) },
                    Clause::Delete { detach: kind == "detach_delete", vars: vec!["d".to_string()] },
                ])
            }
            "for_insert" => {
                let base = self.new_id() * 10;
                self.next_id += 10;
                let values: Vec<Expr> = (1..=self.rng.range(1, 3)).map(|offset| Expr::Lit(Lit::Int(base + offset))).collect();
                let node_label = self.rng.pick(&self.schema.nodes).label;
                let node = NodePat {
                    var: None,
                    label: Some(LabelExpr::Name(node_label.to_string())),
                    props: vec![("id".to_string(), Expr::Var("x".to_string()))],
                    filter: None,
                };
                linear(vec![
                    Clause::For { var: "x".to_string(), list: Expr::List(values), position: None },
                    Clause::Insert(vec![PathPat { var: None, search: None, mode: None, start: node, hops: vec![] }]),
                ])
            }
            _ => {
                // next_set: the statement after NEXT sets a property of the nodes the first one returned.
                let candidates: Vec<(&'static str, &'static str, Ty)> = self
                    .schema
                    .nodes
                    .iter()
                    .flat_map(|node| node.props.iter().filter(|prop| !prop.required).map(move |prop| (node.label, prop.name, prop.ty)))
                    .collect();
                let (mut node_label, mut prop_name, mut prop_ty) = *self.rng.pick(&candidates);
                if !self.literal_allowed(prop_ty) {
                    let plain: Vec<_> = candidates.iter().copied().filter(|(_, _, ty)| !ty.temporal()).collect();
                    (node_label, prop_name, prop_ty) = *self.rng.pick(&plain);
                }
                let path = PathPat {
                    var: None,
                    search: None,
                    mode: None,
                    start: NodePat { var: Some("q".to_string()), label: Some(LabelExpr::Name(node_label.to_string())), ..Default::default() },
                    hops: vec![],
                };
                let first = Linear {
                    clauses: vec![Clause::Match {
                        optional: false,
                        mode: None,
                        paths: vec![path],
                        filter: Some(Expr::Bin(
                            BinOp::Le,
                            Box::new(Expr::Prop("q".to_string(), "id".to_string())),
                            Box::new(Expr::Lit(Lit::Int(self.rng.range(1, 6)))),
                        )),
                    }],
                    tail: Tail::Return(Return {
                        distinct: false,
                        items: ReturnItems::Items(vec![(Expr::Var("q".to_string()), Some("q".to_string()))]),
                        group_by: vec![],
                        order: vec![],
                        offset: None,
                        limit: None,
                    }),
                };
                let value = Expr::Lit(prop_ty.random(&mut self.rng));
                let second = Linear { clauses: vec![Clause::Set(vec![SetItem::Prop("q".to_string(), prop_name.to_string(), value)])], tail: Tail::None };
                Query::Next(vec![first, second])
            }
        }
    }
}

pub fn contains_aggregate(expr: &Expr) -> bool {
    match expr {
        Expr::Agg { .. } => true,
        Expr::Bin(_, left, right) => contains_aggregate(left) || contains_aggregate(right),
        Expr::Not(inner) | Expr::Neg(inner) | Expr::IsNull(inner, _) | Expr::Cast(inner, _) | Expr::Typed(inner, _, _) | Expr::Normalized(inner, _) => {
            contains_aggregate(inner)
        }
        Expr::Func(_, args) | Expr::List(args) => args.iter().any(contains_aggregate),
        Expr::Case(arms, otherwise) => {
            arms.iter().any(|(when, then)| contains_aggregate(when) || contains_aggregate(then)) || otherwise.as_deref().is_some_and(contains_aggregate)
        }
        _ => false,
    }
}
