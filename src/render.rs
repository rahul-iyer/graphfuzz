//! Prints the AST in a dialect profile's syntax. Whether a dialect can express a query is decided from the
//! query's features and the profile's unsupported list; this module only spells what it is given.

use std::sync::OnceLock;

use crate::ast::*;
use crate::profile::{Profile, Quantifier};

/// The built-in GQL profile, for reports and for DuckGQL.
pub fn gql_profile() -> &'static Profile {
    static GQL: OnceLock<Profile> = OnceLock::new();
    GQL.get_or_init(|| Profile::load("gql", std::path::Path::new("."), &[], &[], None).expect("the built-in GQL profile"))
}

pub fn gql(query: &Query) -> String {
    render(query, gql_profile())
}

pub fn render(query: &Query, profile: &Profile) -> String {
    let mut printer = Printer { profile, scope: false };
    format!("{}{}", profile.prefix, printer.query(query))
}

struct Printer<'a> {
    profile: &'a Profile,
    /// Whether the clauses so far bound a variable, which `WITH *` needs.
    scope: bool,
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

impl Printer<'_> {
    fn with_style(&self) -> bool {
        self.profile.syntax.linear_as_with
    }

    fn query(&mut self, query: &Query) -> String {
        match query {
            Query::Linear(linear) => {
                self.scope = false;
                self.linear(linear)
            }
            Query::Composite(left, op, right) => {
                let op = match op {
                    SetOp::Union => "UNION",
                    SetOp::UnionAll => "UNION ALL",
                    SetOp::Except => "EXCEPT",
                    SetOp::ExceptAll => "EXCEPT ALL",
                    SetOp::Intersect => "INTERSECT",
                    SetOp::Otherwise => "OTHERWISE",
                };
                let left = self.query(left);
                let right = self.query(right);
                format!("{left} {op} {right}")
            }
            Query::Next(parts) => {
                self.scope = false;
                let mut printed = Vec::new();
                for (index, part) in parts.iter().enumerate() {
                    let last = index + 1 == parts.len();
                    if self.with_style() && !last {
                        // The statement's result becomes the next statement's input: RETURN becomes WITH.
                        let mut text = part.clauses.iter().map(|clause| self.clause(clause)).collect::<Vec<_>>().join(" ");
                        if let Tail::Return(ret) = &part.tail {
                            text.push(' ');
                            text.push_str(&self.return_statement(ret).replacen("RETURN", "WITH", 1));
                        }
                        printed.push(text);
                        self.scope = true;
                    } else {
                        printed.push(self.linear(part));
                    }
                }
                printed.join(if self.with_style() { " " } else { " NEXT " })
            }
        }
    }

    fn linear(&mut self, linear: &Linear) -> String {
        let mut parts: Vec<String> = linear.clauses.iter().map(|clause| self.clause(clause)).collect();
        match &linear.tail {
            Tail::Return(ret) => parts.push(self.return_statement(ret)),
            Tail::Finish => parts.push("FINISH".to_string()),
            Tail::None => {}
        }
        parts.join(" ")
    }

    /// `WITH *`, or `WITH` when nothing is bound yet.
    fn with_all(&self) -> &'static str {
        if self.scope { "WITH *," } else { "WITH" }
    }

    fn clause(&mut self, clause: &Clause) -> String {
        match clause {
            Clause::Match { optional, mode, paths, filter } => {
                let mut text = String::new();
                if *optional {
                    text.push_str("OPTIONAL ");
                }
                text.push_str("MATCH ");
                match mode {
                    Some(MatchMode::RepeatableElements) => text.push_str("REPEATABLE ELEMENTS "),
                    Some(MatchMode::DifferentEdges) => {
                        text.push_str(&self.profile.syntax.different_edges);
                        text.push(' ');
                    }
                    None => {}
                }
                let paths: Vec<String> = paths.iter().map(|path| self.path(path)).collect();
                text.push_str(&paths.join(", "));
                if let Some(filter) = filter {
                    text.push_str(" WHERE ");
                    text.push_str(&self.expr(filter));
                }
                self.scope = true;
                text
            }
            Clause::Let(bindings) => {
                let text = if self.with_style() {
                    let items: Vec<String> = bindings.iter().map(|(name, value)| format!("{} AS {}", self.expr(value), name)).collect();
                    let with = if self.scope { "WITH *, " } else { "WITH " };
                    format!("{with}{}", items.join(", "))
                } else {
                    let items: Vec<String> = bindings.iter().map(|(name, value)| format!("{} = {}", name, self.expr(value))).collect();
                    format!("LET {}", items.join(", "))
                };
                self.scope = true;
                text
            }
            Clause::For { var, list, position } => {
                let text = if self.with_style() {
                    format!("UNWIND {} AS {}", self.expr(list), var)
                } else {
                    let mut text = format!("FOR {} IN {}", var, self.expr(list));
                    if let Some((offset, name)) = position {
                        text.push_str(&format!(" WITH {} {}", if *offset { "OFFSET" } else { "ORDINALITY" }, name));
                    }
                    text
                };
                self.scope = true;
                text
            }
            Clause::Filter(condition) => {
                if self.with_style() {
                    format!("WITH * WHERE {}", self.expr(condition))
                } else {
                    format!("FILTER {}", self.expr(condition))
                }
            }
            Clause::OrderPage { order, offset, limit } => {
                let mut text = if self.with_style() { "WITH * ".to_string() } else { String::new() };
                if !order.is_empty() {
                    text.push_str(&self.order(order));
                }
                if let Some(offset) = offset {
                    text.push_str(&format!(" {} {offset}", self.profile.syntax.offset));
                }
                if let Some(limit) = limit {
                    text.push_str(&format!(" LIMIT {limit}"));
                }
                text.trim().to_string()
            }
            Clause::Call { optional, scope, body } => {
                let mut text = String::new();
                if *optional {
                    text.push_str("OPTIONAL ");
                }
                text.push_str("CALL ");
                match scope {
                    Some(scope) => text.push_str(&format!("({}) ", scope.join(", "))),
                    None if !self.profile.syntax.call_all.is_empty() => {
                        text.push_str(&self.profile.syntax.call_all);
                        text.push(' ');
                    }
                    None => {}
                }
                let outer = self.scope;
                let body = self.query(body).trim_start_matches(self.profile.prefix.as_str()).to_string();
                self.scope = outer;
                text.push_str(&format!("{{ {body} }}"));
                text
            }
            Clause::Insert(paths) => {
                let paths: Vec<String> = paths.iter().map(|path| self.path(path)).collect();
                self.scope = true;
                format!("{} {}", self.profile.syntax.insert, paths.join(", "))
            }
            Clause::Set(items) => {
                let items: Vec<String> = items
                    .iter()
                    .map(|item| match item {
                        SetItem::Prop(var, prop, value) => format!("{}.{} = {}", var, prop, self.expr(value)),
                        SetItem::Label(var, label) => format!("{var}:{label}"),
                        SetItem::AllProps(var, props) => format!("{} = {}", var, self.map(props)),
                    })
                    .collect();
                format!("SET {}", items.join(", "))
            }
            Clause::Remove(items) => {
                let items: Vec<String> = items
                    .iter()
                    .map(|item| match item {
                        RemoveItem::Prop(var, prop) => format!("{var}.{prop}"),
                        RemoveItem::Label(var, label) => format!("{var}:{label}"),
                    })
                    .collect();
                format!("REMOVE {}", items.join(", "))
            }
            Clause::Delete { detach, vars } => format!("{}DELETE {}", if *detach { "DETACH " } else { "" }, vars.join(", ")),
        }
    }

    fn map(&mut self, props: &[(String, Expr)]) -> String {
        let pairs: Vec<String> = props.iter().map(|(name, value)| format!("{}: {}", name, self.expr(value))).collect();
        format!("{{{}}}", pairs.join(", "))
    }

    fn path(&mut self, path: &PathPat) -> String {
        let mut text = String::new();
        if let Some(var) = &path.var {
            text.push_str(&format!("{var} = "));
        }
        match path.search {
            Some(PathSearch::AnyShortest) => text.push_str("ANY SHORTEST "),
            Some(PathSearch::AllShortest) => text.push_str("ALL SHORTEST "),
            Some(PathSearch::Any(count)) => text.push_str(&format!("ANY {count} ")),
            Some(PathSearch::ShortestK(count)) => text.push_str(&format!("SHORTEST {count} ")),
            Some(PathSearch::ShortestKGroups(count)) => text.push_str(&format!("SHORTEST {count} GROUPS ")),
            Some(PathSearch::All) => text.push_str("ALL "),
            None => {}
        }
        match path.mode {
            Some(PathMode::Walk) => text.push_str("WALK "),
            Some(PathMode::Trail) => text.push_str("TRAIL "),
            Some(PathMode::Acyclic) => text.push_str("ACYCLIC "),
            Some(PathMode::Simple) => text.push_str("SIMPLE "),
            None => {}
        }
        text.push_str(&self.node(&path.start));
        for (edge, node) in &path.hops {
            text.push_str(&self.edge(edge));
            text.push_str(&self.node(node));
        }
        text
    }

    fn filler(&mut self, var: &Option<String>, label: &Option<LabelExpr>, quantifier: &str, props: &[(String, Expr)], filter: &Option<Expr>) -> String {
        let mut text = var.clone().unwrap_or_default();
        if let Some(label) = label {
            text.push(':');
            text.push_str(&self.label(label));
        }
        text.push_str(quantifier);
        if !props.is_empty() {
            text.push(' ');
            text.push_str(&self.map(props));
        }
        if let Some(filter) = filter {
            text.push_str(" WHERE ");
            text.push_str(&self.expr(filter));
        }
        text
    }

    fn node(&mut self, node: &NodePat) -> String {
        format!("({})", self.filler(&node.var, &node.label, "", &node.props, &node.filter))
    }

    fn edge(&mut self, edge: &EdgePat) -> String {
        let star = self.profile.syntax.quantifier == Quantifier::Star;
        let inner = match (edge.quant, star) {
            (Some(Quant::Fixed(count)), true) => format!("*{count}"),
            (Some(Quant::Range(low, high)), true) => format!("*{low}..{high}"),
            (Some(Quant::Plus), true) => "*1..".to_string(),
            (Some(Quant::Star), true) => "*0..".to_string(),
            _ => String::new(),
        };
        let filler = self.filler(&edge.var, &edge.label, &inner, &edge.props, &edge.filter);
        let (open, close) = match edge.dir {
            Dir::Right => ("-[", "]->"),
            Dir::Left => ("<-[", "]-"),
            Dir::Any => ("-[", "]-"),
            // Only directed edges either way; a language whose edges are all directed writes that as any direction.
            Dir::LeftOrRight => {
                if self.profile.name == "gql" {
                    ("<-[", "]->")
                } else {
                    ("-[", "]-")
                }
            }
            Dir::Undirected => ("~[", "]~"),
            Dir::LeftOrUndirected => ("<~[", "]~"),
            Dir::UndirectedOrRight => ("~[", "]~>"),
        };
        let quant = match (edge.quant, star) {
            (Some(Quant::Fixed(count)), false) => format!("{{{count}}}"),
            (Some(Quant::Range(low, high)), false) => format!("{{{low},{high}}}"),
            (Some(Quant::Plus), false) => "+".to_string(),
            (Some(Quant::Star), false) => "*".to_string(),
            _ => String::new(),
        };
        format!("{open}{filler}{close}{quant}")
    }

    fn label(&mut self, label: &LabelExpr) -> String {
        let colon = self.profile.syntax.labeled == "colon";
        match label {
            LabelExpr::Name(name) => name.clone(),
            // openCypher writes a conjunction of labels as :A:B.
            LabelExpr::And(left, right) if colon => format!("{}:{}", self.label(left), self.label(right)),
            LabelExpr::Or(left, right) if colon => format!("{}|{}", self.label(left), self.label(right)),
            LabelExpr::And(left, right) => format!("({}&{})", self.label(left), self.label(right)),
            LabelExpr::Or(left, right) => format!("({}|{})", self.label(left), self.label(right)),
            LabelExpr::Not(inner) => format!("!{}", self.label(inner)),
            LabelExpr::Wildcard => "%".to_string(),
        }
    }

    /// n IS LABELED expr as a boolean over n:A tests, for languages without IS LABELED.
    fn label_test(&mut self, var: &str, label: &LabelExpr) -> String {
        match label {
            LabelExpr::Name(name) => format!("{var}:{name}"),
            LabelExpr::And(left, right) => format!("({} AND {})", self.label_test(var, left), self.label_test(var, right)),
            LabelExpr::Or(left, right) => format!("({} OR {})", self.label_test(var, left), self.label_test(var, right)),
            LabelExpr::Not(inner) => format!("(NOT {})", self.label_test(var, inner)),
            LabelExpr::Wildcard => "true".to_string(),
        }
    }

    fn order(&mut self, order: &[OrderItem]) -> String {
        let items: Vec<String> = order
            .iter()
            .map(|item| {
                let mut text = self.expr(&item.expr);
                if item.desc {
                    text.push_str(" DESC");
                }
                // Neo4j puts nulls last going up and first going down; GQL says so explicitly.
                if self.profile.syntax.explicit_nulls {
                    text.push_str(if item.desc { " NULLS FIRST" } else { " NULLS LAST" });
                }
                text
            })
            .collect();
        format!("ORDER BY {}", items.join(", "))
    }

    fn return_statement(&mut self, ret: &Return) -> String {
        let mut text = "RETURN ".to_string();
        if ret.distinct {
            text.push_str("DISTINCT ");
        }
        match &ret.items {
            ReturnItems::Star => text.push('*'),
            ReturnItems::Items(items) => {
                let items: Vec<String> = items
                    .iter()
                    .map(|(expr, alias)| match alias {
                        Some(alias) => format!("{} AS {}", self.expr(expr), alias),
                        None => self.expr(expr),
                    })
                    .collect();
                text.push_str(&items.join(", "));
            }
        }
        if !ret.group_by.is_empty() {
            text.push_str(&format!(" GROUP BY {}", ret.group_by.join(", ")));
        }
        if !ret.order.is_empty() {
            text.push(' ');
            text.push_str(&self.order(&ret.order));
        }
        if let Some(offset) = ret.offset {
            text.push_str(&format!(" {} {offset}", self.profile.syntax.offset));
        }
        if let Some(limit) = ret.limit {
            text.push_str(&format!(" LIMIT {limit}"));
        }
        text
    }

    fn literal(&mut self, literal: &Lit) -> String {
        match literal {
            Lit::Int(value) => value.to_string(),
            Lit::Float(value) => {
                if self.profile.syntax.float_exponent {
                    format!("{value:?}E0")
                } else {
                    format!("{value:?}")
                }
            }
            Lit::Str(value) => quote(value),
            Lit::Bool(value) => value.to_string().to_uppercase(),
            Lit::Null => "NULL".to_string(),
            Lit::Date(value) => format!("DATE {}", quote(value)),
            Lit::Duration(value) => format!("DURATION {}", quote(value)),
        }
    }

    fn expr(&mut self, expr: &Expr) -> String {
        match expr {
            Expr::Lit(literal) => self.literal(literal),
            Expr::Var(name) => name.clone(),
            Expr::Prop(var, prop) => format!("{var}.{prop}"),
            Expr::Param(name) => format!("${name}"),
            Expr::Bin(op, left, right) => {
                let symbol = match op {
                    BinOp::Add => "+",
                    BinOp::Sub => "-",
                    BinOp::Mul => "*",
                    BinOp::Div => "/",
                    BinOp::Concat => self.profile.syntax.concat.as_str(),
                    BinOp::Eq => "=",
                    BinOp::Ne => "<>",
                    BinOp::Lt => "<",
                    BinOp::Le => "<=",
                    BinOp::Gt => ">",
                    BinOp::Ge => ">=",
                    BinOp::And => "AND",
                    BinOp::Or => "OR",
                    BinOp::Xor => "XOR",
                }
                .to_string();
                format!("({} {} {})", self.expr(left), symbol, self.expr(right))
            }
            Expr::Not(inner) => format!("(NOT {})", self.expr(inner)),
            Expr::Neg(inner) => format!("(-{})", self.expr(inner)),
            Expr::IsNull(inner, negated) => format!("({} IS {}NULL)", self.expr(inner), if *negated { "NOT " } else { "" }),
            Expr::Func(name, args) => self.function(name, args),
            Expr::Agg { name, distinct, arg } => {
                let argument = match arg {
                    Some(arg) => format!("{}{}", if *distinct { "DISTINCT " } else { "" }, self.expr(arg)),
                    None => "*".to_string(),
                };
                let function = self.profile.function(name).to_string();
                if name == "sum" && self.profile.syntax.sum_empty_is_zero {
                    // GQL's sum of no values is NULL.
                    format!("CASE count({argument}) WHEN 0 THEN NULL ELSE {function}({argument}) END")
                } else {
                    format!("{function}({argument})")
                }
            }
            Expr::Case(arms, otherwise) => {
                let mut text = "CASE".to_string();
                for (when, then) in arms {
                    text.push_str(&format!(" WHEN {} THEN {}", self.expr(when), self.expr(then)));
                }
                if let Some(otherwise) = otherwise {
                    text.push_str(&format!(" ELSE {}", self.expr(otherwise)));
                }
                text.push_str(" END");
                text
            }
            Expr::SimpleCase(operand, arms, otherwise) => {
                let mut text = format!("CASE {}", self.expr(operand));
                for (when, then) in arms {
                    text.push_str(&format!(" WHEN {} THEN {}", self.expr(when), self.expr(then)));
                }
                if let Some(otherwise) = otherwise {
                    text.push_str(&format!(" ELSE {}", self.expr(otherwise)));
                }
                text.push_str(" END");
                text
            }
            Expr::List(items) => {
                let items: Vec<String> = items.iter().map(|item| self.expr(item)).collect();
                format!("[{}]", items.join(", "))
            }
            Expr::Index(list, index) => format!("{}[{}]", self.expr(list), self.expr(index)),
            Expr::Record(fields) => {
                let fields: Vec<String> = fields.iter().map(|(name, value)| format!("{}: {}", name, self.expr(value))).collect();
                format!("{{{}}}", fields.join(", "))
            }
            Expr::Cast(inner, ty) => {
                if self.profile.syntax.cast == "gql" {
                    format!("CAST({} AS {})", self.expr(inner), ty)
                } else {
                    let function = match ty.as_str() {
                        "STRING" => "toString",
                        "FLOAT" => "toFloat",
                        "BOOLEAN" => "toBoolean",
                        _ => "toInteger",
                    };
                    format!("{}({})", function, self.expr(inner))
                }
            }
            Expr::Labeled(var, label, negated) => {
                if self.profile.syntax.labeled == "colon" {
                    let test = self.label_test(var, label);
                    if *negated { format!("(NOT {test})") } else { format!("({test})") }
                } else {
                    format!("({} IS {}LABELED {})", var, if *negated { "NOT " } else { "" }, self.label(label))
                }
            }
            Expr::Typed(inner, ty, negated) => format!("({} IS {}TYPED {})", self.expr(inner), if *negated { "NOT " } else { "" }, ty),
            Expr::Normalized(inner, negated) => format!("({} IS {}NORMALIZED)", self.expr(inner), if *negated { "NOT " } else { "" }),
            Expr::Directed(var, negated) => format!("({} IS {}DIRECTED)", var, if *negated { "NOT " } else { "" }),
            Expr::SourceOf(node, edge, negated) => format!("({} IS {}SOURCE OF {})", node, if *negated { "NOT " } else { "" }, edge),
            Expr::DestinationOf(node, edge, negated) => format!("({} IS {}DESTINATION OF {})", node, if *negated { "NOT " } else { "" }, edge),
            Expr::AllDifferent(vars) => format!("ALL_DIFFERENT({})", vars.join(", ")),
            Expr::Same(vars) => format!("SAME({})", vars.join(", ")),
            Expr::PropertyExists(var, prop) => {
                if self.profile.syntax.property_exists == "function" {
                    format!("PROPERTY_EXISTS({var}, {prop})")
                } else {
                    format!("({var}.{prop} IS NOT NULL)")
                }
            }
            Expr::Exists(body) => match body.as_ref() {
                ExistsBody::Pattern(paths, _) => {
                    let paths: Vec<String> = paths.iter().map(|path| self.path(path)).collect();
                    format!("EXISTS {{ {} }}", paths.join(", "))
                }
                ExistsBody::Match(paths, condition) => {
                    let paths: Vec<String> = paths.iter().map(|path| self.path(path)).collect();
                    let mut text = format!("EXISTS {{ MATCH {}", paths.join(", "));
                    if let Some(condition) = condition {
                        text.push_str(&format!(" WHERE {}", self.expr(condition)));
                    }
                    text.push_str(" }");
                    text
                }
            },
        }
    }

    fn function(&mut self, name: &str, args: &[Expr]) -> String {
        let printed: Vec<String> = args.iter().map(|arg| self.expr(arg)).collect();
        if let Some(operator) = self.profile.syntax.operators.get(name) {
            return format!("({} {} {})", printed[0], operator, printed[1]);
        }
        if name == "nullif" {
            return if self.profile.syntax.nullif == "case" {
                format!("CASE WHEN {} = {} THEN NULL ELSE {} END", printed[0], printed[1], printed[0])
            } else {
                format!("{}({})", self.profile.syntax.nullif, printed.join(", "))
            };
        }
        let function = self.profile.function(name);
        let function = if self.profile.name == "gql" && name == "size" { "SIZE" } else { function };
        format!("{}({})", function, printed.join(", "))
    }
}
