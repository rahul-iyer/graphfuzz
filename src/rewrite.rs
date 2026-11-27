//! Rewrites of a query that must not change its answer, for the metamorphic oracles: equivalent graph
//! patterns (as GraphGenie and GRev rewrite them) and NoREC's unoptimizable form of a filter.
//!
//! Each returns None when the query has nothing the rewrite applies to, or when the rewrite would not be
//! equivalent (a path search, a path mode spanning the split, an OPTIONAL MATCH where rows are not additive).

use crate::ast::*;
use crate::generate::contains_aggregate;

/// How a rewrite's answer is compared with the original's.
pub enum Compare {
    /// The rows themselves.
    Rows(Linear),
    /// count(*) below the RETURN: the original's count equals the sum of the variants' counts.
    CountSum(Vec<Linear>),
    /// The original's count(*) below the RETURN equals this query's single value.
    Count(Linear),
}

pub struct Rewrite {
    pub oracle: &'static str,
    pub compare: Compare,
}

/// Every pattern rewrite and NoREC form that applies to the query.
pub fn rewrites(linear: &Linear) -> Vec<Rewrite> {
    let mut result = Vec::new();
    let rows = |oracle, rewritten: Option<Linear>| rewritten.map(|linear| Rewrite { oracle, compare: Compare::Rows(linear) });
    result.extend(rows("split_path", split_path(linear)));
    result.extend(rows("inline_filter", inline_filter(linear)));
    result.extend(rows("redundant_node", redundant_node(linear)));
    if let Some(parts) = unroll(linear) {
        result.push(Rewrite { oracle: "unroll", compare: Compare::CountSum(parts) });
    }
    if let Some(query) = norec(linear) {
        result.push(Rewrite { oracle: "norec", compare: Compare::Count(query) });
    }
    result
}

/// The variable names a query binds or reads, so a fresh name collides with none.
fn names(linear: &Linear) -> Vec<String> {
    fn path(path: &PathPat, out: &mut Vec<String>) {
        out.extend(path.var.clone());
        out.extend(path.start.var.clone());
        for (edge, node) in &path.hops {
            out.extend(edge.var.clone());
            out.extend(node.var.clone());
        }
    }
    let mut out = Vec::new();
    for clause in &linear.clauses {
        match clause {
            Clause::Match { paths, .. } | Clause::Insert(paths) => paths.iter().for_each(|each| path(each, &mut out)),
            Clause::Let(bindings) => out.extend(bindings.iter().map(|(name, _)| name.clone())),
            Clause::For { var, position, .. } => {
                out.push(var.clone());
                out.extend(position.as_ref().map(|(_, name)| name.clone()));
            }
            _ => {}
        }
    }
    out
}

fn fresh(linear: &Linear, taken: &mut Vec<String>) -> String {
    let all = names(linear);
    let name = (0..).map(|index| format!("zz{index}")).find(|name| !all.contains(name) && !taken.contains(name)).unwrap();
    taken.push(name.clone());
    name
}

/// RETURN * lists every variable, so a rewrite that names an anonymous element would add a column.
fn star(linear: &Linear) -> bool {
    matches!(&linear.tail, Tail::Return(Return { items: ReturnItems::Star, .. }))
}

/// A path of two edges or more, split at a node into two paths of the same MATCH that share it:
/// `(a)-[]->(b)-[]->(c)` as `(a)-[]->(b), (b)-[]->(c)`. Both match modes treat the paths of one MATCH as one
/// pattern, so neither which edges may repeat nor the bindings change.
pub fn split_path(linear: &Linear) -> Option<Linear> {
    let mut result = linear.clone();
    let mut taken = Vec::new();
    for clause in &mut result.clauses {
        let Clause::Match { paths, .. } = clause else { continue };
        let Some(index) = paths.iter().position(|path| path.hops.len() >= 2 && path.var.is_none() && path.search.is_none() && path.mode.is_none()) else {
            continue;
        };
        let path = &mut paths[index];
        let middle = path.hops.len() / 2;
        let join = &mut path.hops[middle - 1].1;
        if join.var.is_none() {
            if star(linear) {
                return None;
            }
            join.var = Some(fresh(linear, &mut taken));
        }
        let start = NodePat { var: join.var.clone(), ..Default::default() };
        let rest = path.hops.split_off(middle);
        paths.insert(index + 1, PathPat { var: None, search: None, mode: None, start, hops: rest });
        return Some(result);
    }
    None
}

/// The first MATCH with a property map or an element's own WHERE, with those written in the MATCH's WHERE:
/// `(n:A {p: 1})` as `(n:A) WHERE n.p = 1`. Quantified edges are left alone: their map holds for each edge.
pub fn inline_filter(linear: &Linear) -> Option<Linear> {
    fn take(
        var: &mut Option<String>,
        props: &mut Vec<(String, Expr)>,
        filter: &mut Option<Expr>,
        name: &mut dyn FnMut() -> Option<String>,
        out: &mut Vec<Expr>,
    ) -> bool {
        if props.is_empty() && filter.is_none() {
            return true;
        }
        if var.is_none() {
            match name() {
                Some(fresh) => *var = Some(fresh),
                None => return false,
            }
        }
        let var = var.clone().unwrap();
        for (prop, value) in props.drain(..) {
            out.push(Expr::Bin(BinOp::Eq, Box::new(Expr::Prop(var.clone(), prop)), Box::new(value)));
        }
        out.extend(filter.take());
        true
    }
    let star = star(linear);
    let mut result = linear.clone();
    let mut taken = Vec::new();
    let mut name = || if star { None } else { Some(fresh(linear, &mut taken)) };
    for clause in &mut result.clauses {
        let Clause::Match { paths, filter, .. } = clause else { continue };
        let mut conditions = Vec::new();
        for path in paths.iter_mut() {
            let start = &mut path.start;
            if !take(&mut start.var, &mut start.props, &mut start.filter, &mut name, &mut conditions) {
                return None;
            }
            for (edge, node) in path.hops.iter_mut() {
                if edge.quant.is_none() && !take(&mut edge.var, &mut edge.props, &mut edge.filter, &mut name, &mut conditions) {
                    return None;
                }
                if !take(&mut node.var, &mut node.props, &mut node.filter, &mut name, &mut conditions) {
                    return None;
                }
            }
        }
        if conditions.is_empty() {
            continue;
        }
        let mut condition = conditions.into_iter().reduce(|left, right| Expr::Bin(BinOp::And, Box::new(left), Box::new(right))).unwrap();
        if let Some(existing) = filter.take() {
            condition = Expr::Bin(BinOp::And, Box::new(existing), Box::new(condition));
        }
        *filter = Some(condition);
        return Some(result);
    }
    None
}

/// A MATCH with a path to a named node, with that node again as a pattern of its own: `MATCH (a)-[]->(b)` as
/// `MATCH (a)-[]->(b), (a)`. A node pattern binds no edge, so it only joins on what is bound.
pub fn redundant_node(linear: &Linear) -> Option<Linear> {
    let mut result = linear.clone();
    for clause in &mut result.clauses {
        let Clause::Match { paths, .. } = clause else { continue };
        let named = paths
            .iter()
            .filter(|path| !path.hops.is_empty())
            .find_map(|path| std::iter::once(&path.start).chain(path.hops.iter().map(|(_, node)| node)).find_map(|node| node.var.clone()));
        if let Some(var) = named {
            paths.push(PathPat { var: None, search: None, mode: None, start: NodePat { var: Some(var), ..Default::default() }, hops: vec![] });
            return Some(result);
        }
    }
    None
}

/// Whether the clauses from `index` on produce rows from each input row independently, so the count of the
/// whole is the sum over any split of the rows coming in: no paging, no aggregation, no subquery.
fn row_wise(clauses: &[Clause]) -> bool {
    clauses.iter().all(|clause| match clause {
        Clause::OrderPage { offset, limit, .. } => offset.is_none() && limit.is_none(),
        Clause::Let(bindings) => !bindings.iter().any(|(_, value)| contains_aggregate(value)),
        Clause::Call { .. } | Clause::Insert(_) | Clause::Set(_) | Clause::Remove(_) | Clause::Delete { .. } => false,
        _ => true,
    })
}

/// A bounded quantifier `{low,high}` as one query per length `{k}`: the lengths split the bindings, so their
/// counts add up to the whole's. Not under OPTIONAL MATCH, whose empty match is a row of its own.
pub fn unroll(linear: &Linear) -> Option<Vec<Linear>> {
    for (index, clause) in linear.clauses.iter().enumerate() {
        let Clause::Match { optional: false, paths, .. } = clause else { continue };
        if !row_wise(&linear.clauses[index + 1..]) {
            return None;
        }
        for (path_index, path) in paths.iter().enumerate() {
            if path.search.is_some() {
                continue;
            }
            for (hop, (edge, _)) in path.hops.iter().enumerate() {
                let Some(Quant::Range(low, high)) = edge.quant else { continue };
                if high < low || high - low > 4 {
                    continue;
                }
                let parts = (low..=high)
                    .map(|length| {
                        let mut part = linear.clone();
                        if let Clause::Match { paths, .. } = &mut part.clauses[index] {
                            paths[path_index].hops[hop].0.quant = Some(Quant::Fixed(length));
                        }
                        part
                    })
                    .collect();
                return Some(parts);
            }
        }
    }
    None
}

/// NoREC: `MATCH ... WHERE p` counted, against the same clauses without the filter and
/// `RETURN count(CASE WHEN p THEN 1 END)`, which an optimizer cannot push down or answer from an index.
pub fn norec(linear: &Linear) -> Option<Linear> {
    let index = linear.clauses.iter().rposition(|clause| match clause {
        Clause::Match { optional: false, filter: Some(filter), .. } | Clause::Filter(filter) => !contains_aggregate(filter),
        _ => false,
    })?;
    if !row_wise(&linear.clauses[index + 1..]) {
        return None;
    }
    let mut clauses = linear.clauses.clone();
    let predicate = match &mut clauses[index] {
        Clause::Match { filter, .. } => filter.take().unwrap(),
        Clause::Filter(filter) => {
            let predicate = filter.clone();
            clauses.remove(index);
            predicate
        }
        _ => unreachable!(),
    };
    let counted = Expr::Case(vec![(predicate, Expr::Lit(Lit::Int(1)))], None);
    Some(Linear {
        clauses,
        tail: Tail::Return(Return {
            distinct: false,
            items: ReturnItems::Items(vec![(Expr::Agg { name: "count".to_string(), distinct: false, arg: Some(Box::new(counted)) }, Some("c0".to_string()))]),
            group_by: vec![],
            order: vec![],
            offset: None,
            limit: None,
        }),
    })
}
