//! The features a query uses, read from its AST, so reduced queries are described as exactly as
//! generated ones.

use std::collections::BTreeSet;

use crate::ast::*;

pub fn of(query: &Query) -> BTreeSet<&'static str> {
    let mut found = Found { features: BTreeSet::new(), edges: BTreeSet::new() };
    found.query(query);
    found.features
}

struct Found {
    features: BTreeSet<&'static str>,
    /// Edge variables, to tell a deleted edge from a deleted node.
    edges: BTreeSet<String>,
}

impl Found {
    fn add(&mut self, feature: &'static str) {
        self.features.insert(feature);
    }

    fn query(&mut self, query: &Query) {
        match query {
            Query::Linear(linear) => self.linear(linear, false),
            Query::Composite(left, op, right) => {
                self.add(match op {
                    SetOp::Union => "union",
                    SetOp::UnionAll => "union_all",
                    SetOp::Except | SetOp::ExceptAll => "except",
                    SetOp::Intersect => "intersect",
                    SetOp::Otherwise => "otherwise",
                });
                self.query(left);
                self.query(right);
            }
            Query::Next(parts) => {
                self.add("next");
                for (index, part) in parts.iter().enumerate() {
                    self.linear(part, index > 0);
                    if index > 0 && part.clauses.iter().any(|clause| matches!(clause, Clause::Set(_))) {
                        self.add("next_set");
                    }
                }
            }
        }
    }

    fn linear(&mut self, linear: &Linear, after_next: bool) {
        let matches = linear.clauses.iter().filter(|clause| matches!(clause, Clause::Match { .. })).count();
        if matches > 1 {
            self.add("multi_match");
        }
        if !after_next && !matches!(linear.clauses.first(), Some(Clause::Match { .. }) | None) {
            self.add("matchless");
        }
        let inserts = linear.clauses.iter().filter(|clause| matches!(clause, Clause::Insert(_))).count();
        if inserts > 1 {
            self.add("multi_insert");
        }
        let mut saw_match = false;
        let mut saw_for = false;
        for clause in &linear.clauses {
            match clause {
                Clause::Match { optional, mode, paths, filter } => {
                    saw_match = true;
                    if *optional {
                        self.add("optional_match");
                    }
                    match mode {
                        Some(MatchMode::RepeatableElements) => self.add("match_repeatable"),
                        Some(MatchMode::DifferentEdges) => self.add("match_different"),
                        None => {}
                    }
                    if paths.len() > 1 {
                        self.add("multi_path");
                    }
                    for path in paths {
                        self.path(path);
                    }
                    if let Some(filter) = filter {
                        self.expr(filter);
                    }
                }
                Clause::Let(bindings) => {
                    self.add("let");
                    for (_, value) in bindings {
                        self.expr(value);
                    }
                }
                Clause::For { list, position, .. } => {
                    saw_for = true;
                    self.add("for");
                    match position {
                        Some((true, _)) => self.add("for_offset"),
                        Some((false, _)) => self.add("for_ordinality"),
                        None => {}
                    }
                    self.expr(list);
                }
                Clause::Filter(condition) => {
                    self.add("filter");
                    self.expr(condition);
                }
                Clause::OrderPage { order, .. } => {
                    self.add("order_page_mid");
                    for item in order {
                        self.expr(&item.expr);
                    }
                }
                Clause::Call { optional, scope, body } => {
                    self.add("inline_call");
                    if *optional {
                        self.add("optional_call");
                    }
                    if scope.is_none() {
                        self.add("call_no_scope");
                    }
                    self.query(body);
                }
                Clause::Insert(paths) => {
                    if saw_match {
                        self.add("match_insert");
                    } else if saw_for {
                        self.add("for_insert");
                    } else if paths.iter().any(|path| !path.hops.is_empty()) {
                        self.add("insert_path");
                    } else {
                        self.add("insert_node");
                    }
                    for path in paths {
                        self.path(path);
                    }
                }
                Clause::Set(items) => {
                    self.add("set_prop");
                    for item in items {
                        if let SetItem::Prop(_, _, value) = item {
                            self.expr(value);
                        }
                    }
                }
                Clause::Remove(_) => self.add("remove_prop"),
                Clause::Delete { detach, vars } => {
                    if *detach {
                        self.add("detach_delete");
                    } else if vars.iter().all(|var| self.edges.contains(var)) {
                        self.add("delete_edge");
                    } else {
                        self.add("delete_attached");
                    }
                }
            }
        }
        match &linear.tail {
            Tail::Return(ret) => {
                if ret.distinct {
                    self.add("return_distinct");
                }
                if !ret.group_by.is_empty() {
                    self.add("group_by");
                }
                if !ret.order.is_empty() {
                    self.add("order_by");
                    if ret.order.iter().any(|item| item.desc) {
                        self.add("order_desc");
                    }
                }
                if ret.limit.is_some() {
                    self.add("limit");
                }
                if ret.offset.is_some() {
                    self.add("offset");
                }
                match &ret.items {
                    ReturnItems::Star => self.add("return_star"),
                    ReturnItems::Items(items) => {
                        for (expr, _) in items {
                            self.expr(expr);
                        }
                    }
                }
            }
            Tail::Finish => self.add("finish"),
            Tail::None => {}
        }
    }

    fn label(&mut self, label: &LabelExpr) {
        match label {
            LabelExpr::Name(_) => {}
            LabelExpr::And(left, right) => {
                self.add("label_and");
                self.label(left);
                self.label(right);
            }
            LabelExpr::Or(left, right) => {
                self.add("label_or");
                self.label(left);
                self.label(right);
            }
            LabelExpr::Not(inner) => {
                self.add("label_not");
                self.label(inner);
            }
            LabelExpr::Wildcard => self.add("label_wildcard"),
        }
    }

    fn path(&mut self, path: &PathPat) {
        if path.var.is_some() {
            self.add("path_var");
        }
        match path.search {
            Some(PathSearch::AnyShortest) => self.add("any_shortest"),
            Some(PathSearch::AllShortest) => self.add("all_shortest"),
            Some(PathSearch::Any(_)) => self.add("any_k"),
            Some(PathSearch::ShortestK(_)) => self.add("shortest_k"),
            Some(PathSearch::ShortestKGroups(_)) => self.add("shortest_k_groups"),
            Some(PathSearch::All) | None => {}
        }
        match path.mode {
            Some(PathMode::Walk) => self.add("path_walk"),
            Some(PathMode::Trail) => self.add("path_trail"),
            Some(PathMode::Simple) => self.add("path_simple"),
            Some(PathMode::Acyclic) => self.add("path_acyclic"),
            None => {}
        }
        if path.hops.len() >= 2 {
            self.add("two_hops");
        }
        self.node(&path.start);
        for (edge, node) in &path.hops {
            self.edge(edge);
            self.node(node);
        }
    }

    fn node(&mut self, node: &NodePat) {
        match &node.label {
            Some(label) => self.label(label),
            None if node.var.is_none() || node.props.is_empty() => {}
            None => {}
        }
        if !node.props.is_empty() {
            self.add("property_map");
            for (_, value) in &node.props {
                self.expr(value);
            }
        }
        if let Some(filter) = &node.filter {
            self.add("element_where");
            self.expr(filter);
        }
    }

    fn edge(&mut self, edge: &EdgePat) {
        match edge.dir {
            Dir::Right => {}
            Dir::Left => self.add("edge_left"),
            Dir::Any => self.add("edge_any"),
            Dir::LeftOrRight => self.add("edge_left_or_right"),
            Dir::Undirected => self.add("edge_undirected"),
            Dir::LeftOrUndirected => self.add("edge_left_or_undirected"),
            Dir::UndirectedOrRight => self.add("edge_undirected_or_right"),
        }
        match &edge.label {
            Some(label) => self.label(label),
            None => self.add("unlabeled_edge"),
        }
        if let Some(var) = &edge.var {
            self.add("edge_var");
            self.edges.insert(var.clone());
        }
        match edge.quant {
            Some(Quant::Fixed(_)) => self.add("quant_fixed"),
            Some(Quant::Range(..)) => self.add("quant_range"),
            Some(Quant::Plus) => self.add("quant_plus"),
            Some(Quant::Star) => self.add("quant_star"),
            None => {}
        }
        if !edge.props.is_empty() {
            self.add("property_map");
        }
        if let Some(filter) = &edge.filter {
            self.add("element_where");
            self.expr(filter);
        }
    }

    fn expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Lit(Lit::Float(_)) => self.add("float_literal"),
            Expr::Lit(Lit::Str(_)) => self.add("string_literal"),
            Expr::Lit(_) | Expr::Var(_) | Expr::Prop(..) | Expr::Param(_) => {}
            Expr::Bin(op, left, right) => {
                match op {
                    BinOp::Add | BinOp::Sub | BinOp::Mul => self.add("arith"),
                    BinOp::Div => self.add("div"),
                    BinOp::Concat => self.add("concat"),
                    BinOp::Or => self.add("or"),
                    BinOp::Xor => self.add("xor"),
                    _ => {}
                }
                self.expr(left);
                self.expr(right);
            }
            Expr::Not(inner) => {
                self.add("not");
                self.expr(inner);
            }
            Expr::Neg(inner) => {
                self.add("neg");
                self.expr(inner);
            }
            Expr::IsNull(inner, _) => {
                self.add("is_null");
                self.expr(inner);
            }
            Expr::Func(name, args) => {
                let feature = match name.as_str() {
                    "coalesce" => Some("coalesce"),
                    "nullif" => Some("nullif"),
                    "upper" | "lower" | "trim" | "left" | "right" => Some("string_fn"),
                    "char_length" => Some("char_length"),
                    "abs" | "ceil" | "floor" | "sqrt" => Some("numeric_fn"),
                    "mod" => Some("mod"),
                    "power" => Some("power"),
                    "sin" | "cos" => Some("trig"),
                    "exp" | "ln" | "log10" => Some("log_fn"),
                    "size" => Some("size"),
                    "path_length" => Some("path_length"),
                    "element_id" => Some("element_id"),
                    _ => None,
                };
                if let Some(feature) = feature {
                    self.add(feature);
                }
                for arg in args {
                    self.expr(arg);
                }
            }
            Expr::Agg { name, distinct, arg } => {
                self.add(match (name.as_str(), *distinct, arg.is_some()) {
                    ("count", _, false) => "agg_count",
                    ("count", true, _) => "agg_count_distinct",
                    ("count", false, true) => "agg_count_expr",
                    ("sum", ..) => "agg_sum",
                    ("avg", ..) => "agg_avg",
                    ("min" | "max", ..) => "agg_min_max",
                    _ => "agg_collect",
                });
                if let Some(arg) = arg {
                    self.expr(arg);
                }
            }
            Expr::Case(arms, otherwise) => {
                self.add("case");
                for (when, then) in arms {
                    self.expr(when);
                    self.expr(then);
                }
                if let Some(otherwise) = otherwise {
                    self.expr(otherwise);
                }
            }
            Expr::SimpleCase(operand, arms, otherwise) => {
                self.add("simple_case");
                self.expr(operand);
                for (when, then) in arms {
                    self.expr(when);
                    self.expr(then);
                }
                if let Some(otherwise) = otherwise {
                    self.expr(otherwise);
                }
            }
            Expr::List(items) => {
                self.add("list");
                for item in items {
                    self.expr(item);
                }
            }
            Expr::Index(list, index) => {
                self.add("list");
                self.expr(list);
                self.expr(index);
            }
            Expr::Record(fields) => {
                for (_, value) in fields {
                    self.expr(value);
                }
            }
            Expr::Cast(inner, _) => {
                self.add("cast");
                self.expr(inner);
            }
            Expr::Labeled(_, label, _) => {
                self.add("is_labeled");
                self.label(label);
            }
            Expr::Typed(inner, _, _) => {
                self.add("is_typed");
                self.expr(inner);
            }
            Expr::Normalized(inner, _) => {
                self.add("is_normalized");
                self.expr(inner);
            }
            Expr::Directed(..) => self.add("is_directed"),
            Expr::SourceOf(..) | Expr::DestinationOf(..) => self.add("source_of"),
            Expr::AllDifferent(_) => self.add("all_different"),
            Expr::Same(_) => self.add("same"),
            Expr::PropertyExists(..) => self.add("property_exists"),
            Expr::Exists(body) => match body.as_ref() {
                ExistsBody::Pattern(paths, _) => {
                    self.add("exists_pattern");
                    for path in paths {
                        self.path(path);
                    }
                }
                ExistsBody::Match(paths, condition) => {
                    self.add("exists_match");
                    for path in paths {
                        self.path(path);
                    }
                    if let Some(condition) = condition {
                        self.expr(condition);
                    }
                }
            },
        }
    }
}
