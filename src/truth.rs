//! The right answer, computed from the fixture graph itself: how many bindings a query's clauses produce.
//! graphfuzz writes the data, so for simple queries it can count matches without any engine, and catch a
//! bug the reference shares with the engine under test.
//!
//! It covers MATCH and OPTIONAL MATCH over directed and any-direction edges, label expressions, property
//! maps, element and clause WHERE, bounded quantifiers, path modes, FILTER and LET, with three-valued logic
//! over property values. Anything else (functions, subqueries, paging, path searches, unbounded quantifiers)
//! is not counted: `count` returns None and the oracle is not applied.
//!
//! Bindings are counted as distinct assignments of every element of a pattern, named or not. An
//! any-direction edge pattern over a self-loop is one binding, as the path it matches is the same either way.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use crate::ast::*;
use crate::generate::contains_aggregate;
use crate::schema::{Fixture, FixtureNode};

/// More bindings than this, or more partial matches explored, and the query is not counted.
const LIMIT: usize = 200_000;

#[derive(Clone, Debug)]
struct Element {
    label: &'static str,
    props: BTreeMap<&'static str, Lit>,
}

/// The fixture as a graph: nodes, edges, and each node's outgoing and incoming edges.
pub struct Graph {
    nodes: Vec<Element>,
    edges: Vec<(Element, usize, usize)>,
    outgoing: Vec<Vec<usize>>,
    incoming: Vec<Vec<usize>>,
}

impl Graph {
    /// None if the fixture has an undirected edge, which this does not model.
    pub fn new(fixture: &Fixture) -> Option<Graph> {
        let id = |node: &FixtureNode| {
            node.props.iter().find_map(|(name, value)| match (name, value) {
                (&"id", Lit::Int(id)) => Some(*id),
                _ => None,
            })
        };
        let mut index = BTreeMap::new();
        let mut nodes = Vec::new();
        for node in &fixture.nodes {
            index.insert((node.label, id(node)?), nodes.len());
            nodes.push(Element { label: node.label, props: node.props.iter().cloned().collect() });
        }
        let mut outgoing = vec![Vec::new(); nodes.len()];
        let mut incoming = vec![Vec::new(); nodes.len()];
        let mut edges = Vec::new();
        for edge in &fixture.edges {
            if edge.undirected {
                return None;
            }
            let (source, target) = (*index.get(&edge.source)?, *index.get(&edge.target)?);
            outgoing[source].push(edges.len());
            incoming[target].push(edges.len());
            edges.push((Element { label: edge.label, props: edge.props.iter().cloned().collect() }, source, target));
        }
        Some(Graph { nodes, edges, outgoing, incoming })
    }

    /// How many bindings the clauses produce, when every MATCH that names no match mode uses distinct edges
    /// (`different_edges`, as Cypher) or may repeat them (as GQL's REPEATABLE ELEMENTS); None if the query
    /// uses anything this does not model.
    pub fn count(&self, clauses: &[Clause], different_edges: bool) -> Option<u64> {
        let mut counter = Counter { graph: self, different_edges, work: 0 };
        counter.clauses(clauses, &Env::new()).ok()
    }
}

#[derive(Clone, Debug)]
enum V {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// An exact number; compared as a double, and only with another.
    Decimal(f64),
    Str(String),
    /// A temporal value of one kind, by its ISO text, which orders as the value does: dates, local datetimes,
    /// and zoned datetimes (all in UTC).
    Temporal(u8, String),
    Node(usize),
    Edge(usize),
    /// A path or a group of edges: bound, but not modelled.
    Opaque,
}

fn value(lit: &Lit) -> Result<V, ()> {
    Ok(match lit {
        Lit::Int(value) => V::Int(*value),
        Lit::Float(value) => V::Float(*value),
        Lit::Str(value) => V::Str(value.clone()),
        Lit::Bool(value) => V::Bool(*value),
        Lit::Null => V::Null,
        Lit::Date(text) => V::Temporal(0, text.clone()),
        Lit::LocalDateTime(text) => V::Temporal(1, text.clone()),
        Lit::ZonedDateTime(text) => V::Temporal(2, text.clone()),
        Lit::Decimal(text) => V::Decimal(text.parse().map_err(|_| ())?),
        Lit::Duration(_) => return Err(()),
    })
}

type Env = BTreeMap<String, V>;

/// Not modelled: the query is not counted.
type Res<T> = Result<T, ()>;

fn compare(left: &V, right: &V) -> Res<Option<Ordering>> {
    Ok(match (left, right) {
        (V::Null, _) | (_, V::Null) => None,
        (V::Int(a), V::Int(b)) => Some(a.cmp(b)),
        (V::Int(a), V::Float(b)) => (*a as f64).partial_cmp(b),
        (V::Float(a), V::Int(b)) => a.partial_cmp(&(*b as f64)),
        (V::Float(a), V::Float(b)) | (V::Decimal(a), V::Decimal(b)) => a.partial_cmp(b),
        (V::Str(a), V::Str(b)) => Some(a.cmp(b)),
        (V::Bool(a), V::Bool(b)) => Some(a.cmp(b)),
        (V::Temporal(a, x), V::Temporal(b, y)) if a == b => Some(x.cmp(y)),
        _ => return Err(()),
    })
}

fn truth(value: &V) -> Res<Option<bool>> {
    match value {
        V::Bool(value) => Ok(Some(*value)),
        V::Null => Ok(None),
        _ => Err(()),
    }
}

fn boolean(value: Option<bool>) -> V {
    value.map(V::Bool).unwrap_or(V::Null)
}

fn label_matches(label: &LabelExpr, actual: &str) -> bool {
    match label {
        LabelExpr::Name(name) => name == actual,
        LabelExpr::And(left, right) => label_matches(left, actual) && label_matches(right, actual),
        LabelExpr::Or(left, right) => label_matches(left, actual) || label_matches(right, actual),
        LabelExpr::Not(inner) => !label_matches(inner, actual),
        LabelExpr::Wildcard => true,
    }
}

struct Counter<'g> {
    graph: &'g Graph,
    different_edges: bool,
    work: usize,
}

/// A partial match of one path: the bindings so far, the edges the MATCH has used, and the path's elements.
#[derive(Clone)]
struct State {
    env: Env,
    used: Vec<usize>,
    at: usize,
    nodes: Vec<usize>,
    edges: Vec<usize>,
}

impl Counter<'_> {
    fn tick(&mut self, amount: usize) -> Res<()> {
        self.work += amount;
        if self.work > LIMIT { Err(()) } else { Ok(()) }
    }

    fn element(&self, value: &V) -> Option<&Element> {
        match value {
            V::Node(index) => Some(&self.graph.nodes[*index]),
            V::Edge(index) => Some(&self.graph.edges[*index].0),
            _ => None,
        }
    }

    fn eval(&self, expr: &Expr, env: &Env) -> Res<V> {
        Ok(match expr {
            Expr::Lit(lit) => value(lit)?,
            Expr::Var(name) => env.get(name).cloned().ok_or(())?,
            Expr::Prop(var, prop) => match env.get(var).ok_or(())? {
                V::Null => V::Null,
                other => match self.element(other) {
                    Some(element) => element.props.get(prop.as_str()).map(value).transpose()?.unwrap_or(V::Null),
                    None => return Err(()),
                },
            },
            Expr::Bin(op, left, right) => {
                let (a, b) = (self.eval(left, env)?, self.eval(right, env)?);
                match op {
                    BinOp::And | BinOp::Or | BinOp::Xor => {
                        let (a, b) = (truth(&a)?, truth(&b)?);
                        boolean(match op {
                            BinOp::And => match (a, b) {
                                (Some(false), _) | (_, Some(false)) => Some(false),
                                (Some(true), Some(true)) => Some(true),
                                _ => None,
                            },
                            BinOp::Or => match (a, b) {
                                (Some(true), _) | (_, Some(true)) => Some(true),
                                (Some(false), Some(false)) => Some(false),
                                _ => None,
                            },
                            _ => a.zip(b).map(|(a, b)| a != b),
                        })
                    }
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                        let order = match (&a, &b) {
                            // Elements compare by identity, and only for equality.
                            (V::Node(x), V::Node(y)) | (V::Edge(x), V::Edge(y)) if matches!(op, BinOp::Eq | BinOp::Ne) => Some(x.cmp(y)),
                            _ => compare(&a, &b)?,
                        };
                        boolean(order.map(|order| match op {
                            BinOp::Eq => order == Ordering::Equal,
                            BinOp::Ne => order != Ordering::Equal,
                            BinOp::Lt => order == Ordering::Less,
                            BinOp::Le => order != Ordering::Greater,
                            BinOp::Gt => order == Ordering::Greater,
                            _ => order != Ordering::Less,
                        }))
                    }
                    BinOp::Add | BinOp::Sub | BinOp::Mul => match (a, b) {
                        (V::Null, _) | (_, V::Null) => V::Null,
                        (V::Int(x), V::Int(y)) => V::Int(
                            match op {
                                BinOp::Add => x.checked_add(y),
                                BinOp::Sub => x.checked_sub(y),
                                _ => x.checked_mul(y),
                            }
                            .ok_or(())?,
                        ),
                        (x @ (V::Int(_) | V::Float(_)), y @ (V::Int(_) | V::Float(_))) => {
                            let float = |v: V| match v {
                                V::Int(i) => i as f64,
                                V::Float(f) => f,
                                _ => unreachable!(),
                            };
                            let (x, y) = (float(x), float(y));
                            V::Float(match op {
                                BinOp::Add => x + y,
                                BinOp::Sub => x - y,
                                _ => x * y,
                            })
                        }
                        _ => return Err(()),
                    },
                    BinOp::Concat => match (a, b) {
                        (V::Null, _) | (_, V::Null) => V::Null,
                        (V::Str(x), V::Str(y)) => V::Str(x + &y),
                        _ => return Err(()),
                    },
                    // Integer division and its rounding differ between languages.
                    BinOp::Div => return Err(()),
                }
            }
            Expr::Not(inner) => boolean(truth(&self.eval(inner, env)?)?.map(|value| !value)),
            Expr::Neg(inner) => match self.eval(inner, env)? {
                V::Null => V::Null,
                V::Int(value) => V::Int(value.checked_neg().ok_or(())?),
                V::Float(value) => V::Float(-value),
                _ => return Err(()),
            },
            Expr::IsNull(inner, negated) => V::Bool(matches!(self.eval(inner, env)?, V::Null) != *negated),
            Expr::Case(arms, otherwise) => {
                for (when, then) in arms {
                    if truth(&self.eval(when, env)?)? == Some(true) {
                        return self.eval(then, env);
                    }
                }
                match otherwise {
                    Some(otherwise) => self.eval(otherwise, env)?,
                    None => V::Null,
                }
            }
            Expr::Labeled(var, label, negated) => match env.get(var).ok_or(())? {
                V::Null => V::Null,
                other => match self.element(other) {
                    Some(element) => V::Bool(label_matches(label, element.label) != *negated),
                    None => return Err(()),
                },
            },
            _ => return Err(()),
        })
    }

    /// Whether a condition holds: true, not false or unknown.
    fn holds(&self, condition: &Expr, env: &Env) -> Res<bool> {
        Ok(truth(&self.eval(condition, env)?)? == Some(true))
    }

    /// Whether an element fits its pattern's label, property map, and WHERE, with its variable bound.
    fn fits(&self, element: &Element, label: &Option<LabelExpr>, props: &[(String, Expr)], filter: &Option<Expr>, env: &Env) -> Res<bool> {
        if let Some(label) = label
            && !label_matches(label, element.label)
        {
            return Ok(false);
        }
        for (prop, expected) in props {
            let actual = element.props.get(prop.as_str()).map(value).transpose()?.unwrap_or(V::Null);
            if compare(&actual, &self.eval(expected, env)?)? != Some(Ordering::Equal) {
                return Ok(false);
            }
        }
        match filter {
            Some(filter) => self.holds(filter, env),
            None => Ok(true),
        }
    }

    /// Binds a node pattern to `node`, if it fits and agrees with what its variable is bound to.
    fn node(&self, pattern: &NodePat, node: usize, env: &Env) -> Res<Option<Env>> {
        let mut env = env.clone();
        if let Some(var) = &pattern.var {
            match env.get(var) {
                Some(V::Node(bound)) if *bound != node => return Ok(None),
                Some(V::Node(_)) => {}
                // Bound to NULL by an OPTIONAL MATCH: nothing matches it.
                Some(V::Null) => return Ok(None),
                Some(_) => return Err(()),
                None => {
                    env.insert(var.clone(), V::Node(node));
                }
            }
        }
        Ok(if self.fits(&self.graph.nodes[node], &pattern.label, &pattern.props, &pattern.filter, &env)? { Some(env) } else { None })
    }

    /// The edges a step in this direction can take from a node, and the node each leads to.
    fn steps(&self, from: usize, dir: Dir) -> Res<Vec<(usize, usize)>> {
        let out = || self.graph.outgoing[from].iter().map(|&edge| (edge, self.graph.edges[edge].2));
        let back = || self.graph.incoming[from].iter().map(|&edge| (edge, self.graph.edges[edge].1));
        Ok(match dir {
            Dir::Right => out().collect(),
            Dir::Left => back().collect(),
            // A self-loop is one step either way, not two.
            Dir::Any | Dir::LeftOrRight => out().chain(back().filter(|&(_, node)| node != from)).collect(),
            _ => return Err(()),
        })
    }

    /// Every way to extend a partial match by one edge pattern and the node pattern after it.
    fn hop(&mut self, state: &State, edge: &EdgePat, node: &NodePat) -> Res<Vec<State>> {
        let (low, high) = match edge.quant {
            None => (1, 1),
            Some(Quant::Fixed(count)) => (count, count),
            Some(Quant::Range(low, high)) if low <= high => (low, high),
            _ => return Err(()),
        };
        if let Some(var) = &edge.var
            && state.env.contains_key(var)
        {
            // An edge variable bound twice: engines differ on what that means.
            return Err(());
        }
        // Walk `high` steps from the state, keeping those at each length from `low` on.
        let mut ends = Vec::new();
        let mut frontier = vec![state.clone()];
        for length in 0..=high {
            if length >= low {
                ends.extend(frontier.iter().cloned());
            }
            if length == high {
                break;
            }
            let mut next = Vec::new();
            for current in &frontier {
                for (taken, to) in self.steps(current.at, edge.dir)? {
                    self.tick(1)?;
                    if self.different_edges && current.used.contains(&taken) {
                        continue;
                    }
                    let mut env = current.env.clone();
                    if let Some(var) = &edge.var {
                        env.insert(var.clone(), V::Edge(taken));
                    }
                    if !self.fits(&self.graph.edges[taken].0, &edge.label, &edge.props, &edge.filter, &env)? {
                        continue;
                    }
                    let mut moved = current.clone();
                    moved.used.push(taken);
                    moved.edges.push(taken);
                    moved.nodes.push(to);
                    moved.at = to;
                    next.push(moved);
                }
            }
            frontier = next;
        }
        let mut result = Vec::new();
        for mut end in ends {
            if let Some(var) = &edge.var {
                end.env.insert(var.clone(), if edge.quant.is_some() { V::Opaque } else { V::Edge(*end.edges.last().unwrap()) });
            }
            if let Some(env) = self.node(node, end.at, &end.env)? {
                end.env = env;
                result.push(end);
            }
        }
        self.tick(result.len())?;
        Ok(result)
    }

    fn path(&mut self, path: &PathPat, env: &Env, used: &[usize]) -> Res<Vec<(Env, Vec<usize>)>> {
        if path.search.is_some() {
            return Err(());
        }
        let starts: Vec<usize> = match path.start.var.as_ref().and_then(|var| env.get(var)) {
            Some(V::Node(node)) => vec![*node],
            Some(V::Null) => vec![],
            Some(_) => return Err(()),
            None => (0..self.graph.nodes.len()).collect(),
        };
        let mut states = Vec::new();
        for start in starts {
            if let Some(env) = self.node(&path.start, start, env)? {
                states.push(State { env, used: used.to_vec(), at: start, nodes: vec![start], edges: vec![] });
            }
        }
        for (edge, node) in &path.hops {
            let mut next = Vec::new();
            for state in &states {
                next.extend(self.hop(state, edge, node)?);
            }
            states = next;
        }
        let distinct = |items: &[usize]| {
            let mut sorted = items.to_vec();
            sorted.sort_unstable();
            sorted.windows(2).all(|pair| pair[0] != pair[1])
        };
        states.retain(|state| match path.mode {
            None | Some(PathMode::Walk) => true,
            Some(PathMode::Trail) => distinct(&state.edges),
            Some(PathMode::Acyclic) => distinct(&state.nodes),
            Some(PathMode::Simple) => {
                let closed = state.nodes.len() > 1 && state.nodes.first() == state.nodes.last();
                distinct(if closed { &state.nodes[1..] } else { &state.nodes })
            }
        });
        Ok(states
            .into_iter()
            .map(|mut state| {
                if let Some(var) = &path.var {
                    state.env.insert(var.clone(), V::Opaque);
                }
                (state.env, state.used)
            })
            .collect())
    }

    /// The variables a MATCH binds that are not bound yet, for the NULL row of an OPTIONAL MATCH.
    fn new_vars(paths: &[PathPat], env: &Env) -> Vec<String> {
        let mut vars = Vec::new();
        for path in paths {
            vars.extend(path.var.clone());
            vars.extend(path.start.var.clone());
            for (edge, node) in &path.hops {
                vars.extend(edge.var.clone());
                vars.extend(node.var.clone());
            }
        }
        vars.retain(|var| !env.contains_key(var));
        vars
    }

    fn clauses(&mut self, clauses: &[Clause], env: &Env) -> Res<u64> {
        let Some((clause, rest)) = clauses.split_first() else { return Ok(1) };
        self.tick(1)?;
        match clause {
            Clause::Match { optional, mode, paths, filter } => {
                let saved = self.different_edges;
                self.different_edges = match mode {
                    Some(MatchMode::DifferentEdges) => true,
                    Some(MatchMode::RepeatableElements) => false,
                    None => saved,
                };
                let mut bindings = vec![(env.clone(), Vec::new())];
                for path in paths {
                    let mut next = Vec::new();
                    for (env, used) in &bindings {
                        next.extend(self.path(path, env, used)?);
                    }
                    bindings = next;
                }
                self.different_edges = saved;
                let mut total = 0u64;
                let mut matched = false;
                for (bound, _) in bindings {
                    if let Some(filter) = filter
                        && !self.holds(filter, &bound)?
                    {
                        continue;
                    }
                    matched = true;
                    total += self.clauses(rest, &bound)?;
                }
                if *optional && !matched {
                    let mut empty = env.clone();
                    for var in Counter::new_vars(paths, env) {
                        empty.insert(var, V::Null);
                    }
                    total += self.clauses(rest, &empty)?;
                }
                Ok(total)
            }
            Clause::Filter(condition) => {
                if self.holds(condition, env)? {
                    self.clauses(rest, env)
                } else {
                    Ok(0)
                }
            }
            Clause::Let(bindings) => {
                let mut env = env.clone();
                for (name, expr) in bindings {
                    if contains_aggregate(expr) {
                        return Err(());
                    }
                    let value = self.eval(expr, &env)?;
                    env.insert(name.clone(), value);
                }
                self.clauses(rest, &env)
            }
            Clause::OrderPage { offset: None, limit: None, .. } => self.clauses(rest, env),
            _ => Err(()),
        }
    }
}
