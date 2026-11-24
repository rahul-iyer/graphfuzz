//! A query AST covering the shared core of ISO GQL and Cypher. Printers in `render` turn it into
//! either dialect; constructs one dialect lacks are never generated for it.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Right,
    Left,
    Any,
    Undirected,
    LeftOrUndirected,
    UndirectedOrRight,
    LeftOrRight,
}

impl Dir {
    pub fn reversed(self) -> Dir {
        match self {
            Dir::Right => Dir::Left,
            Dir::Left => Dir::Right,
            Dir::LeftOrUndirected => Dir::UndirectedOrRight,
            Dir::UndirectedOrRight => Dir::LeftOrUndirected,
            other => other,
        }
    }
}

#[derive(Clone, Debug)]
pub enum LabelExpr {
    Name(String),
    And(Box<LabelExpr>, Box<LabelExpr>),
    Or(Box<LabelExpr>, Box<LabelExpr>),
    Not(Box<LabelExpr>),
    Wildcard,
}

#[derive(Clone, Copy, Debug)]
pub enum Quant {
    Fixed(u32),
    Range(u32, u32),
    Plus,
    Star,
}

#[derive(Clone, Debug, Default)]
pub struct NodePat {
    pub var: Option<String>,
    pub label: Option<LabelExpr>,
    pub props: Vec<(String, Expr)>,
    pub filter: Option<Expr>,
}

#[derive(Clone, Debug)]
pub struct EdgePat {
    pub var: Option<String>,
    pub label: Option<LabelExpr>,
    pub dir: Dir,
    pub quant: Option<Quant>,
    pub props: Vec<(String, Expr)>,
    pub filter: Option<Expr>,
}

#[derive(Clone, Copy, Debug)]
pub enum PathSearch {
    AnyShortest,
    AllShortest,
    Any(u32),
    ShortestK(u32),
    ShortestKGroups(u32),
    All,
}

#[derive(Clone, Copy, Debug)]
pub enum PathMode {
    Walk,
    Trail,
    Simple,
    Acyclic,
}

#[derive(Clone, Debug)]
pub struct PathPat {
    pub var: Option<String>,
    pub search: Option<PathSearch>,
    pub mode: Option<PathMode>,
    pub start: NodePat,
    pub hops: Vec<(EdgePat, NodePat)>,
}

impl PathPat {
    /// The same path walked from its last node to its first.
    pub fn reversed(&self) -> PathPat {
        let mut nodes = vec![self.start.clone()];
        nodes.extend(self.hops.iter().map(|(_, node)| node.clone()));
        nodes.reverse();
        let mut edges: Vec<EdgePat> = self.hops.iter().map(|(edge, _)| edge.clone()).collect();
        edges.reverse();
        for edge in &mut edges {
            edge.dir = edge.dir.reversed();
        }
        PathPat {
            var: self.var.clone(),
            search: self.search,
            mode: self.mode,
            start: nodes[0].clone(),
            hops: edges.into_iter().zip(nodes.into_iter().skip(1)).collect(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum MatchMode {
    RepeatableElements,
    DifferentEdges,
}

#[derive(Clone, Debug)]
pub enum Lit {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Null,
    Date(String),
    Duration(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Concat,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Xor,
}

#[derive(Clone, Debug)]
pub enum Expr {
    Lit(Lit),
    Var(String),
    Prop(String, String),
    Param(String),
    Bin(BinOp, Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Neg(Box<Expr>),
    IsNull(Box<Expr>, bool),
    /// A function written name(args): GQL and Cypher spell some names differently, see render.
    Func(String, Vec<Expr>),
    Agg { name: String, distinct: bool, arg: Option<Box<Expr>> },
    /// CASE WHEN c THEN v ... ELSE e END
    Case(Vec<(Expr, Expr)>, Option<Box<Expr>>),
    /// CASE operand WHEN v THEN r ... END
    SimpleCase(Box<Expr>, Vec<(Expr, Expr)>, Option<Box<Expr>>),
    List(Vec<Expr>),
    Index(Box<Expr>, Box<Expr>),
    Record(Vec<(String, Expr)>),
    Cast(Box<Expr>, String),
    Labeled(String, LabelExpr, bool),
    Typed(Box<Expr>, String, bool),
    Normalized(Box<Expr>, bool),
    Directed(String, bool),
    SourceOf(String, String, bool),
    DestinationOf(String, String, bool),
    AllDifferent(Vec<String>),
    Same(Vec<String>),
    PropertyExists(String, String),
    Exists(Box<ExistsBody>),
}

#[derive(Clone, Debug)]
pub enum ExistsBody {
    Pattern(Vec<PathPat>, Option<Expr>),
    Match(Vec<PathPat>, Option<Expr>),
}

#[derive(Clone, Debug)]
pub enum SetItem {
    Prop(String, String, Expr),
    Label(String, String),
    AllProps(String, Vec<(String, Expr)>),
}

#[derive(Clone, Debug)]
pub enum RemoveItem {
    Prop(String, String),
    Label(String, String),
}

#[derive(Clone, Debug)]
pub struct OrderItem {
    pub expr: Expr,
    pub desc: bool,
    pub nulls: Option<bool>,
}

#[derive(Clone, Debug)]
pub enum Clause {
    Match { optional: bool, mode: Option<MatchMode>, paths: Vec<PathPat>, filter: Option<Expr> },
    Let(Vec<(String, Expr)>),
    For { var: String, list: Expr, position: Option<(bool, String)> },
    Filter(Expr),
    OrderPage { order: Vec<OrderItem>, offset: Option<u32>, limit: Option<u32> },
    Call { optional: bool, scope: Option<Vec<String>>, body: Box<Query> },
    Insert(Vec<PathPat>),
    Set(Vec<SetItem>),
    Remove(Vec<RemoveItem>),
    Delete { detach: bool, vars: Vec<String> },
}

impl Clause {
    pub fn modifies(&self) -> bool {
        matches!(self, Clause::Insert(_) | Clause::Set(_) | Clause::Remove(_) | Clause::Delete { .. })
    }
}

#[derive(Clone, Debug)]
pub enum ReturnItems {
    Star,
    Items(Vec<(Expr, Option<String>)>),
}

#[derive(Clone, Debug)]
pub struct Return {
    pub distinct: bool,
    pub items: ReturnItems,
    pub group_by: Vec<String>,
    pub order: Vec<OrderItem>,
    pub offset: Option<u32>,
    pub limit: Option<u32>,
}

#[derive(Clone, Debug)]
pub enum Tail {
    Return(Return),
    Finish,
    None,
}

#[derive(Clone, Debug)]
pub struct Linear {
    pub clauses: Vec<Clause>,
    pub tail: Tail,
}

#[derive(Clone, Copy, Debug)]
pub enum SetOp {
    Union,
    UnionAll,
    Except,
    ExceptAll,
    Intersect,
    Otherwise,
}

#[derive(Clone, Debug)]
pub enum Query {
    Linear(Linear),
    Composite(Box<Query>, SetOp, Box<Query>),
    /// Statements chained with NEXT; each but the first reads the previous one's columns.
    Next(Vec<Linear>),
}

impl Query {
    pub fn modifies(&self) -> bool {
        match self {
            Query::Linear(linear) => linear.clauses.iter().any(Clause::modifies),
            Query::Composite(left, _, right) => left.modifies() || right.modifies(),
            Query::Next(parts) => parts.iter().any(|part| part.clauses.iter().any(Clause::modifies)),
        }
    }
}
