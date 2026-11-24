//! Shrinks a failing query: tries simpler variants and keeps any that still fails the same way, until
//! none does. The result names the construct behind an error far better than the original query.

use crate::ast::*;

/// Greedily reduces `query` while `still_fails` holds, trying at most `budget` variants.
pub fn reduce(query: &Query, budget: usize, mut still_fails: impl FnMut(&Query) -> bool) -> Query {
    let mut current = query.clone();
    let mut tried = 0;
    'progress: loop {
        for candidate in candidates(&current) {
            if tried >= budget {
                break 'progress;
            }
            tried += 1;
            if still_fails(&candidate) {
                current = candidate;
                continue 'progress;
            }
        }
        break;
    }
    current
}

fn candidates(query: &Query) -> Vec<Query> {
    let mut result = Vec::new();
    match query {
        Query::Linear(linear) => {
            for reduced in linear_candidates(linear) {
                result.push(Query::Linear(reduced));
            }
        }
        Query::Composite(left, op, right) => {
            result.push((**left).clone());
            result.push((**right).clone());
            for reduced in candidates(left) {
                result.push(Query::Composite(Box::new(reduced), *op, right.clone()));
            }
            for reduced in candidates(right) {
                result.push(Query::Composite(left.clone(), *op, Box::new(reduced)));
            }
        }
        Query::Next(parts) => {
            for part in parts {
                result.push(Query::Linear(part.clone()));
            }
            for (index, part) in parts.iter().enumerate() {
                for reduced in linear_candidates(part) {
                    let mut parts = parts.clone();
                    parts[index] = reduced;
                    result.push(Query::Next(parts));
                }
            }
        }
    }
    result
}

/// Simpler expressions to try in place of one: its operands, then a literal.
fn expr_candidates(expr: &Expr) -> Vec<Expr> {
    let mut result = Vec::new();
    match expr {
        Expr::Bin(_, left, right) => {
            result.push((**left).clone());
            result.push((**right).clone());
        }
        Expr::Not(inner) | Expr::Neg(inner) | Expr::IsNull(inner, _) | Expr::Cast(inner, _) | Expr::Typed(inner, _, _) | Expr::Normalized(inner, _) => {
            result.push((**inner).clone())
        }
        Expr::Func(_, args) | Expr::List(args) => result.extend(args.iter().cloned()),
        Expr::Case(arms, otherwise) => {
            for (when, then) in arms {
                result.push(when.clone());
                result.push(then.clone());
            }
            if let Some(otherwise) = otherwise {
                result.push((**otherwise).clone());
            }
        }
        Expr::Agg { arg: Some(_), name, distinct } => result.push(Expr::Agg { name: name.clone(), distinct: *distinct, arg: None }),
        _ => {}
    }
    if !matches!(expr, Expr::Lit(_)) {
        result.push(Expr::Lit(Lit::Bool(true)));
        result.push(Expr::Lit(Lit::Int(1)));
    }
    result
}

fn path_candidates(path: &PathPat) -> Vec<PathPat> {
    let mut result = Vec::new();
    let mut push = |change: &dyn Fn(&mut PathPat)| {
        let mut candidate = path.clone();
        change(&mut candidate);
        result.push(candidate);
    };
    if path.var.is_some() {
        push(&|path| path.var = None);
    }
    if path.search.is_some() {
        push(&|path| path.search = None);
    }
    if path.mode.is_some() {
        push(&|path| path.mode = None);
    }
    if !path.hops.is_empty() {
        push(&|path| {
            path.hops.pop();
        });
    }
    let node_changes = |node: &NodePat| {
        let mut changes: Vec<NodePat> = Vec::new();
        if node.label.is_some() {
            changes.push(NodePat { label: None, ..node.clone() });
        }
        if let Some(LabelExpr::Or(left, _) | LabelExpr::And(left, _)) = &node.label {
            changes.push(NodePat { label: Some((**left).clone()), ..node.clone() });
        }
        if !node.props.is_empty() {
            changes.push(NodePat { props: vec![], ..node.clone() });
        }
        if node.filter.is_some() {
            changes.push(NodePat { filter: None, ..node.clone() });
        }
        changes
    };
    for change in node_changes(&path.start) {
        let mut candidate = path.clone();
        candidate.start = change;
        result.push(candidate);
    }
    for (index, (edge, node)) in path.hops.iter().enumerate() {
        for change in node_changes(node) {
            let mut candidate = path.clone();
            candidate.hops[index].1 = change;
            result.push(candidate);
        }
        let mut edge_changes: Vec<EdgePat> = Vec::new();
        if edge.quant.is_some() {
            edge_changes.push(EdgePat { quant: None, ..edge.clone() });
        }
        if edge.dir != Dir::Right {
            edge_changes.push(EdgePat { dir: Dir::Right, ..edge.clone() });
        }
        if edge.label.is_some() {
            edge_changes.push(EdgePat { label: None, ..edge.clone() });
        }
        if let Some(LabelExpr::Or(left, _) | LabelExpr::And(left, _)) = &edge.label {
            edge_changes.push(EdgePat { label: Some((**left).clone()), ..edge.clone() });
        }
        if !edge.props.is_empty() {
            edge_changes.push(EdgePat { props: vec![], ..edge.clone() });
        }
        if edge.filter.is_some() {
            edge_changes.push(EdgePat { filter: None, ..edge.clone() });
        }
        if edge.var.is_some() {
            edge_changes.push(EdgePat { var: None, ..edge.clone() });
        }
        for change in edge_changes {
            let mut candidate = path.clone();
            candidate.hops[index].0 = change;
            result.push(candidate);
        }
    }
    result
}

fn linear_candidates(linear: &Linear) -> Vec<Linear> {
    let mut result = Vec::new();
    // Whole clauses first: they shrink the query fastest.
    for index in 0..linear.clauses.len() {
        if linear.clauses.len() > 1 || !matches!(linear.tail, Tail::None) {
            let mut candidate = linear.clone();
            candidate.clauses.remove(index);
            result.push(candidate);
        }
    }
    for (index, clause) in linear.clauses.iter().enumerate() {
        let mut replace = |replacement: Clause| {
            let mut candidate = linear.clone();
            candidate.clauses[index] = replacement;
            result.push(candidate);
        };
        match clause {
            Clause::Match { optional, mode, paths, filter } => {
                if *optional {
                    replace(Clause::Match { optional: false, mode: *mode, paths: paths.clone(), filter: filter.clone() });
                }
                if mode.is_some() {
                    replace(Clause::Match { optional: *optional, mode: None, paths: paths.clone(), filter: filter.clone() });
                }
                if let Some(filter) = filter {
                    replace(Clause::Match { optional: *optional, mode: *mode, paths: paths.clone(), filter: None });
                    for simpler in expr_candidates(filter) {
                        replace(Clause::Match { optional: *optional, mode: *mode, paths: paths.clone(), filter: Some(simpler) });
                    }
                }
                if paths.len() > 1 {
                    for skip in 0..paths.len() {
                        let fewer: Vec<PathPat> = paths.iter().enumerate().filter(|(position, _)| *position != skip).map(|(_, path)| path.clone()).collect();
                        replace(Clause::Match { optional: *optional, mode: *mode, paths: fewer, filter: filter.clone() });
                    }
                }
                for (position, path) in paths.iter().enumerate() {
                    for simpler in path_candidates(path) {
                        let mut paths = paths.clone();
                        paths[position] = simpler;
                        replace(Clause::Match { optional: *optional, mode: *mode, paths, filter: filter.clone() });
                    }
                }
            }
            Clause::Filter(condition) => {
                for simpler in expr_candidates(condition) {
                    replace(Clause::Filter(simpler));
                }
            }
            Clause::Let(bindings) => {
                for simpler in expr_candidates(&bindings[0].1) {
                    replace(Clause::Let(vec![(bindings[0].0.clone(), simpler)]));
                }
            }
            Clause::For { var, list, position } => {
                if position.is_some() {
                    replace(Clause::For { var: var.clone(), list: list.clone(), position: None });
                }
            }
            Clause::Call { optional, scope, body } => {
                if *optional {
                    replace(Clause::Call { optional: false, scope: scope.clone(), body: body.clone() });
                }
                for simpler in candidates(body) {
                    replace(Clause::Call { optional: *optional, scope: scope.clone(), body: Box::new(simpler) });
                }
            }
            Clause::OrderPage { order, offset, limit } => {
                if order.len() > 1 {
                    replace(Clause::OrderPage { order: order[..1].to_vec(), offset: *offset, limit: *limit });
                }
            }
            Clause::Insert(paths) => {
                for (position, path) in paths.iter().enumerate() {
                    for simpler in path_candidates(path) {
                        let mut paths = paths.clone();
                        paths[position] = simpler;
                        replace(Clause::Insert(paths));
                    }
                }
            }
            Clause::Set(items) => {
                for (position, item) in items.iter().enumerate() {
                    if let SetItem::Prop(var, prop, value) = item {
                        for simpler in expr_candidates(value) {
                            let mut items = items.clone();
                            items[position] = SetItem::Prop(var.clone(), prop.clone(), simpler);
                            replace(Clause::Set(items));
                        }
                    }
                }
            }
            Clause::Remove(_) | Clause::Delete { .. } => {}
        }
    }
    if let Tail::Return(ret) = &linear.tail {
        let mut replace = |change: &dyn Fn(&mut Return)| {
            let mut candidate = linear.clone();
            if let Tail::Return(ret) = &mut candidate.tail {
                change(ret);
            }
            result.push(candidate);
        };
        if ret.distinct {
            replace(&|ret| ret.distinct = false);
        }
        if !ret.group_by.is_empty() {
            replace(&|ret| ret.group_by.clear());
        }
        if ret.limit.is_some() {
            replace(&|ret| ret.limit = None);
        }
        if ret.offset.is_some() {
            replace(&|ret| ret.offset = None);
        }
        if !ret.order.is_empty() {
            replace(&|ret| ret.order.clear());
        }
        if let ReturnItems::Items(items) = &ret.items {
            if items.len() > 1 {
                for keep in 0..items.len() {
                    let item = items[keep].clone();
                    let alias = item.1.clone();
                    replace(&move |ret| {
                        ret.items = ReturnItems::Items(vec![item.clone()]);
                        ret.group_by.retain(|key| Some(key) == alias.as_ref());
                        ret.order.retain(|order| matches!(&order.expr, Expr::Var(name) if Some(name) == alias.as_ref()));
                    });
                }
            }
            for (position, (expr, alias)) in items.iter().enumerate() {
                for simpler in expr_candidates(expr) {
                    let simpler = simpler.clone();
                    let alias = alias.clone();
                    replace(&move |ret| {
                        if let ReturnItems::Items(items) = &mut ret.items {
                            items[position] = (simpler.clone(), alias.clone());
                        }
                    });
                }
            }
        }
    }
    if matches!(linear.tail, Tail::Finish) {
        let mut candidate = linear.clone();
        candidate.tail = Tail::Return(Return {
            distinct: false,
            items: ReturnItems::Items(vec![(Expr::Lit(Lit::Int(1)), Some("c0".to_string()))]),
            group_by: vec![],
            order: vec![],
            offset: None,
            limit: None,
        });
        result.push(candidate);
    }
    result
}
