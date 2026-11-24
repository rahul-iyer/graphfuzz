//! Prints the AST as ISO GQL, or as Cypher 25 for Neo4j. A construct Neo4j has no counterpart for makes
//! the Cypher printer give up, and the query is then checked against DuckGQL alone.

use crate::ast::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Gql,
    Cypher,
}

pub fn gql(query: &Query) -> String {
    let mut printer = Printer { dialect: Dialect::Gql, supported: true };
    printer.query(query)
}

pub fn cypher(query: &Query) -> Option<String> {
    let mut printer = Printer { dialect: Dialect::Cypher, supported: true };
    let text = printer.query(query);
    printer.supported.then(|| format!("CYPHER 25 {text}"))
}

struct Printer {
    dialect: Dialect,
    supported: bool,
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

impl Printer {
    fn gql(&self) -> bool {
        self.dialect == Dialect::Gql
    }

    /// Marks the query as GQL-only when printing Cypher.
    fn gql_only(&mut self) {
        if !self.gql() {
            self.supported = false;
        }
    }

    fn query(&mut self, query: &Query) -> String {
        match query {
            Query::Linear(linear) => self.linear(linear),
            Query::Composite(left, op, right) => {
                let op = match op {
                    SetOp::Union => "UNION",
                    SetOp::UnionAll => "UNION ALL",
                    SetOp::Except => {
                        self.gql_only();
                        "EXCEPT"
                    }
                    SetOp::ExceptAll => {
                        self.gql_only();
                        "EXCEPT ALL"
                    }
                    SetOp::Intersect => {
                        self.gql_only();
                        "INTERSECT"
                    }
                    SetOp::Otherwise => {
                        self.gql_only();
                        "OTHERWISE"
                    }
                };
                format!("{} {} {}", self.query(left), op, self.query(right))
            }
            Query::Next(parts) => parts.iter().map(|part| self.linear(part)).collect::<Vec<_>>().join(" NEXT "),
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
                    Some(MatchMode::DifferentEdges) => text.push_str(if self.gql() { "DIFFERENT EDGES " } else { "DIFFERENT RELATIONSHIPS " }),
                    None => {}
                }
                let paths: Vec<String> = paths.iter().map(|path| self.path(path)).collect();
                text.push_str(&paths.join(", "));
                if let Some(filter) = filter {
                    text.push_str(" WHERE ");
                    text.push_str(&self.expr(filter));
                }
                text
            }
            Clause::Let(bindings) => {
                let items: Vec<String> = bindings.iter().map(|(name, value)| format!("{} = {}", name, self.expr(value))).collect();
                format!("LET {}", items.join(", "))
            }
            Clause::For { var, list, position } => {
                let mut text = format!("FOR {} IN {}", var, self.expr(list));
                if let Some((offset, name)) = position {
                    self.gql_only();
                    text.push_str(&format!(" WITH {} {}", if *offset { "OFFSET" } else { "ORDINALITY" }, name));
                }
                text
            }
            Clause::Filter(condition) => format!("FILTER {}", self.expr(condition)),
            Clause::OrderPage { order, offset, limit } => {
                let mut text = String::new();
                if !order.is_empty() {
                    text.push_str(&self.order(order));
                }
                if let Some(offset) = offset {
                    text.push_str(&format!(" OFFSET {offset}"));
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
                if let Some(scope) = scope {
                    text.push_str(&format!("({}) ", scope.join(", ")));
                } else if !self.gql() {
                    // Cypher 25 needs a scope clause; `*` imports every variable, as GQL's bare CALL does.
                    text.push_str("(*) ");
                }
                text.push_str(&format!("{{ {} }}", self.query(body)));
                text
            }
            Clause::Insert(paths) => {
                let paths: Vec<String> = paths.iter().map(|path| self.path(path)).collect();
                format!("INSERT {}", paths.join(", "))
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
            Some(PathMode::Simple) => {
                self.gql_only();
                text.push_str("SIMPLE ");
            }
            None => {}
        }
        text.push_str(&self.node(&path.start));
        for (edge, node) in &path.hops {
            text.push_str(&self.edge(edge));
            text.push_str(&self.node(node));
        }
        text
    }

    fn filler(&mut self, var: &Option<String>, label: &Option<LabelExpr>, props: &[(String, Expr)], filter: &Option<Expr>) -> String {
        let mut text = var.clone().unwrap_or_default();
        if let Some(label) = label {
            text.push(':');
            text.push_str(&self.label(label));
        }
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
        format!("({})", self.filler(&node.var, &node.label, &node.props, &node.filter))
    }

    fn edge(&mut self, edge: &EdgePat) -> String {
        let filler = self.filler(&edge.var, &edge.label, &edge.props, &edge.filter);
        let (open, close) = match edge.dir {
            Dir::Right => ("-[", "]->"),
            Dir::Left => ("<-[", "]-"),
            Dir::Any => ("-[", "]-"),
            Dir::LeftOrRight => {
                if self.gql() {
                    ("<-[", "]->")
                } else {
                    // Every Neo4j relationship is directed, so either direction is any direction.
                    ("-[", "]-")
                }
            }
            Dir::Undirected => {
                self.gql_only();
                ("~[", "]~")
            }
            Dir::LeftOrUndirected => {
                self.gql_only();
                ("<~[", "]~")
            }
            Dir::UndirectedOrRight => {
                self.gql_only();
                ("~[", "]~>")
            }
        };
        let quant = match edge.quant {
            Some(Quant::Fixed(count)) => format!("{{{count}}}"),
            Some(Quant::Range(low, high)) => format!("{{{low},{high}}}"),
            Some(Quant::Plus) => "+".to_string(),
            Some(Quant::Star) => "*".to_string(),
            None => String::new(),
        };
        format!("{open}{filler}{close}{quant}")
    }

    fn label(&mut self, label: &LabelExpr) -> String {
        match label {
            LabelExpr::Name(name) => name.clone(),
            LabelExpr::And(left, right) => format!("({}&{})", self.label(left), self.label(right)),
            LabelExpr::Or(left, right) => format!("({}|{})", self.label(left), self.label(right)),
            LabelExpr::Not(inner) => format!("!{}", self.label(inner)),
            LabelExpr::Wildcard => "%".to_string(),
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
                if self.gql() {
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
            ReturnItems::Star => {
                self.gql_only();
                text.push('*');
            }
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
            text.push_str(&format!(" OFFSET {offset}"));
        }
        if let Some(limit) = ret.limit {
            text.push_str(&format!(" LIMIT {limit}"));
        }
        text
    }

    fn literal(&mut self, literal: &Lit) -> String {
        match literal {
            Lit::Int(value) => value.to_string(),
            // GQL reads 1.5 as exact; the exponent makes it approximate, like Cypher's 1.5.
            Lit::Float(value) => {
                if self.gql() {
                    format!("{value:?}E0")
                } else {
                    format!("{value:?}")
                }
            }
            Lit::Str(value) => quote(value),
            Lit::Bool(value) => value.to_string().to_uppercase(),
            Lit::Null => "NULL".to_string(),
            Lit::Date(value) => {
                if self.gql() {
                    format!("DATE {}", quote(value))
                } else {
                    format!("date({})", quote(value))
                }
            }
            Lit::Duration(value) => {
                if self.gql() {
                    format!("DURATION {}", quote(value))
                } else {
                    format!("duration({})", quote(value))
                }
            }
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
                    BinOp::Concat => "||",
                    BinOp::Eq => "=",
                    BinOp::Ne => "<>",
                    BinOp::Lt => "<",
                    BinOp::Le => "<=",
                    BinOp::Gt => ">",
                    BinOp::Ge => ">=",
                    BinOp::And => "AND",
                    BinOp::Or => "OR",
                    BinOp::Xor => "XOR",
                };
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
                match (name.as_str(), self.dialect) {
                    ("collect_list", Dialect::Cypher) => format!("collect({argument})"),
                    // Neo4j's sum of no values is 0; ISO GQL's is NULL.
                    ("sum", Dialect::Cypher) => format!("CASE count({argument}) WHEN 0 THEN NULL ELSE sum({argument}) END"),
                    _ => format!("{name}({argument})"),
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
            Expr::Index(list, index) => {
                self.gql_only();
                format!("{}[{}]", self.expr(list), self.expr(index))
            }
            Expr::Record(fields) => {
                let fields: Vec<String> = fields.iter().map(|(name, value)| format!("{}: {}", name, self.expr(value))).collect();
                format!("{{{}}}", fields.join(", "))
            }
            Expr::Cast(inner, ty) => {
                if self.gql() {
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
            Expr::Labeled(var, label, negated) => format!("({} IS {}LABELED {})", var, if *negated { "NOT " } else { "" }, self.label(label)),
            Expr::Typed(inner, ty, negated) => format!("({} IS {}TYPED {})", self.expr(inner), if *negated { "NOT " } else { "" }, ty),
            Expr::Normalized(inner, negated) => format!("({} IS {}NORMALIZED)", self.expr(inner), if *negated { "NOT " } else { "" }),
            Expr::Directed(var, negated) => format!("({} IS {}DIRECTED)", var, if *negated { "NOT " } else { "" }),
            Expr::SourceOf(node, edge, negated) => {
                self.gql_only();
                format!("({} IS {}SOURCE OF {})", node, if *negated { "NOT " } else { "" }, edge)
            }
            Expr::DestinationOf(node, edge, negated) => {
                self.gql_only();
                format!("({} IS {}DESTINATION OF {})", node, if *negated { "NOT " } else { "" }, edge)
            }
            Expr::AllDifferent(vars) => {
                self.gql_only();
                format!("ALL_DIFFERENT({})", vars.join(", "))
            }
            Expr::Same(vars) => {
                self.gql_only();
                format!("SAME({})", vars.join(", "))
            }
            Expr::PropertyExists(var, prop) => {
                if self.gql() {
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
        match (name, self.dialect) {
            ("mod", Dialect::Cypher) => format!("({} % {})", printed[0], printed[1]),
            ("power", Dialect::Cypher) => format!("({} ^ {})", printed[0], printed[1]),
            ("element_id", Dialect::Cypher) => format!("elementId({})", printed[0]),
            ("nullif", Dialect::Cypher) => format!("nullIf({})", printed.join(", ")),
            ("size", Dialect::Gql) => format!("SIZE({})", printed[0]),
            _ => format!("{}({})", name, printed.join(", ")),
        }
    }
}
