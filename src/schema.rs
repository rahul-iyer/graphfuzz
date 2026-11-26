//! The graph the generator writes queries for, from the `schema:` section of config.yml, and the fixture
//! data every engine loads. Fixture statements are ASTs, so each engine's dialect profile spells them.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use serde::Deserialize;

use crate::ast::*;
use crate::rng::Rng;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    Int,
    Float,
    Str,
    Bool,
    Date,
    LocalDateTime,
    ZonedDateTime,
    Decimal,
}

impl Ty {
    pub fn parse(text: &str) -> Result<Ty> {
        Ok(match text.trim().to_uppercase().as_str() {
            "INT64" | "INT" | "INTEGER" => Ty::Int,
            "FLOAT64" | "FLOAT" | "DOUBLE" => Ty::Float,
            "STRING" => Ty::Str,
            "BOOL" | "BOOLEAN" => Ty::Bool,
            "DATE" => Ty::Date,
            "LOCAL DATETIME" | "TIMESTAMP" => Ty::LocalDateTime,
            "ZONED DATETIME" => Ty::ZonedDateTime,
            "DECIMAL" => Ty::Decimal,
            other => bail!("unknown property type '{other}' (INT64, FLOAT64, STRING, BOOL, DATE, LOCAL DATETIME, ZONED DATETIME, DECIMAL)"),
        })
    }

    /// The type in a GQL graph type.
    pub fn gql_name(self) -> &'static str {
        match self {
            Ty::Int => "INT64",
            Ty::Float => "FLOAT64",
            Ty::Str => "STRING",
            Ty::Bool => "BOOL",
            Ty::Date => "DATE",
            Ty::LocalDateTime => "LOCAL DATETIME",
            Ty::ZonedDateTime => "ZONED DATETIME",
            Ty::Decimal => "DECIMAL(18,3)",
        }
    }

    pub fn temporal(self) -> bool {
        matches!(self, Ty::Date | Ty::LocalDateTime | Ty::ZonedDateTime)
    }

    /// A random value of the type, as a literal.
    pub fn random(self, rng: &mut Rng) -> Lit {
        let date = |rng: &mut Rng| {
            let day = time::Date::from_calendar_date(2020, time::Month::January, 1).unwrap() + time::Duration::days(rng.range(0, 1500));
            format!("{:04}-{:02}-{:02}", day.year(), day.month() as u8, day.day())
        };
        let clock = |rng: &mut Rng| format!("{:02}:{:02}:{:02}", rng.range(0, 23), rng.range(0, 59), rng.range(0, 59));
        match self {
            Ty::Int => Lit::Int(rng.range(1, 60)),
            Ty::Float => Lit::Float(rng.range(0, 400) as f64 / 8.0),
            Ty::Str => Lit::Str(format!("v{}", rng.range(0, 5))),
            Ty::Bool => Lit::Bool(rng.chance(0.5)),
            Ty::Date => Lit::Date(date(rng)),
            Ty::LocalDateTime => Lit::LocalDateTime(format!("{}T{}", date(rng), clock(rng))),
            // Neo4j compares zoned datetimes by offset as well as instant, so every value is in UTC.
            Ty::ZonedDateTime => Lit::ZonedDateTime(format!("{}T{}Z", date(rng), clock(rng))),
            Ty::Decimal => Lit::Decimal(format!("{}.{:03}", rng.range(0, 999), rng.range(0, 999))),
        }
    }
}

pub struct Prop {
    pub name: &'static str,
    pub ty: Ty,
    pub required: bool,
}

pub struct NodeType {
    pub label: &'static str,
    pub props: Vec<Prop>,
    pub count: i64,
}

pub struct EdgeType {
    pub label: &'static str,
    pub source: &'static str,
    pub target: &'static str,
    pub undirected: bool,
    pub props: Vec<Prop>,
    /// How many edges of this type the fixture has.
    pub count: i64,
}

pub struct Schema {
    pub nodes: Vec<NodeType>,
    pub edges: Vec<EdgeType>,
    /// The values the fixture holds, by element label and property, so generated filters compare with data
    /// that exists; empty until `with_values`.
    pub values: BTreeMap<(&'static str, &'static str), Vec<Lit>>,
}

/// The `schema:` section of config.yml. A property type is written as `INT64` or `INT64 NOT NULL`.
#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct SchemaConfig {
    #[serde(default)]
    pub nodes: BTreeMap<String, NodeConfig>,
    #[serde(default)]
    pub edges: BTreeMap<String, EdgeConfig>,
    /// Adds random node types, edge types, and properties to the declared ones, and draws every type's count
    /// and density, all from the seed; see `randomized`.
    #[serde(default)]
    pub random: Option<RandomSchema>,
}

/// Ranges, inclusive, that a random schema is drawn from.
#[derive(Deserialize, Debug, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct RandomSchema {
    /// Node types added to the declared ones.
    pub node_types: [usize; 2],
    /// Edge types added to the declared ones; at least one goes from a node type to itself.
    pub edge_types: [usize; 2],
    /// Properties added to each node and edge type.
    pub properties: [usize; 2],
    /// Nodes of each type.
    pub nodes: [i64; 2],
    /// Edges of each type per node of its source type.
    pub degree: [i64; 2],
}

impl Default for RandomSchema {
    fn default() -> Self {
        RandomSchema { node_types: [1, 3], edge_types: [2, 5], properties: [0, 3], nodes: [3, 15], degree: [1, 3] }
    }
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct NodeConfig {
    /// How many nodes of this type the fixture has; their ids are 1 to count.
    #[serde(default = "node_count")]
    pub count: i64,
    pub properties: BTreeMap<String, String>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct EdgeConfig {
    pub from: String,
    pub to: String,
    /// An undirected edge type; only DuckGQL's GQL-only exploration uses it, as Cypher has none.
    #[serde(default)]
    pub undirected: bool,
    #[serde(default)]
    pub properties: BTreeMap<String, String>,
    /// How many edges of this type the fixture has; by default twice its source type's count, at most 20.
    #[serde(default)]
    pub count: Option<i64>,
}

fn node_count() -> i64 {
    6
}

fn leak(text: &str) -> &'static str {
    // The schema lives for the whole run, and the generator keeps its names as &'static str.
    Box::leak(text.to_string().into_boxed_str())
}

fn properties(properties: &BTreeMap<String, String>, node: bool) -> Result<Vec<Prop>> {
    let mut result = Vec::new();
    // `id` first, so digests list it first.
    let mut names: Vec<&String> = properties.keys().collect();
    names.sort_by_key(|name| (name.as_str() != "id", name.to_string()));
    for name in names {
        let text = &properties[name];
        let upper = text.to_uppercase();
        let required = upper.trim_end().ends_with("NOT NULL");
        let ty = Ty::parse(upper.trim_end().trim_end_matches("NOT NULL"))?;
        if node && name == "id" && (ty != Ty::Int || !required) {
            bail!("node property id must be INT64 NOT NULL: fixture nodes are found by it");
        }
        result.push(Prop { name: leak(name), ty, required });
    }
    if node && !properties.contains_key("id") {
        bail!("every node type needs an id INT64 NOT NULL property");
    }
    Ok(result)
}

impl SchemaConfig {
    /// A social graph with every property type.
    pub fn social() -> SchemaConfig {
        serde_yaml::from_str(
            "
nodes:
  Person:
    count: 12
    properties: {id: INT64 NOT NULL, name: STRING, age: INT64, score: FLOAT64, active: BOOL, born: DATE, joined: LOCAL DATETIME, seen: ZONED DATETIME, balance: DECIMAL}
  City:
    count: 5
    properties: {id: INT64 NOT NULL, name: STRING, population: INT64, established: DATE}
  Company:
    count: 4
    properties: {id: INT64 NOT NULL, name: STRING, founded: INT64, revenue: DECIMAL}
edges:
  KNOWS: {from: Person, to: Person, properties: {since: INT64, weight: FLOAT64, met: DATE}}
  LIVES_IN: {from: Person, to: City, properties: {since: INT64}}
  WORKS_AT: {from: Person, to: Company, properties: {since: INT64, role: STRING, started: LOCAL DATETIME}}
  LOCATED_IN: {from: Company, to: City}
  NEAR: {from: City, to: City, undirected: true, properties: {km: INT64}}
",
        )
        .expect("the default schema")
    }

    /// The schema; `undirected` keeps undirected edge types, which only DuckGQL's exploration graph holds.
    pub fn build(&self, undirected: bool) -> Result<Schema> {
        let mut nodes = Vec::new();
        for (label, node) in &self.nodes {
            nodes.push(NodeType { label: leak(label), props: properties(&node.properties, true)?, count: node.count.max(1) });
        }
        let mut edges = Vec::new();
        for (label, edge) in &self.edges {
            if edge.undirected && !undirected {
                continue;
            }
            for endpoint in [&edge.from, &edge.to] {
                if !self.nodes.contains_key(endpoint) {
                    bail!("edge type {label} names node type {endpoint}, which the schema does not declare");
                }
            }
            let source_count = self.nodes[&edge.from].count.max(1);
            edges.push(EdgeType {
                label: leak(label),
                source: leak(&edge.from),
                target: leak(&edge.to),
                undirected: edge.undirected,
                props: properties(&edge.properties, false)?,
                count: edge.count.unwrap_or((source_count * 2).min(20)).max(0),
            });
        }
        // One name, one type: DuckGQL keeps a property name to one type across the graph, and a query reading a
        // property of an unlabeled element must mean one type.
        let mut types: BTreeMap<&str, Ty> = BTreeMap::new();
        for prop in nodes.iter().flat_map(|node| node.props.iter()).chain(edges.iter().flat_map(|edge| edge.props.iter())) {
            if let Some(ty) = types.insert(prop.name, prop.ty)
                && ty != prop.ty
            {
                bail!("property {} has two types, {} and {}; give each name one type", prop.name, ty.gql_name(), prop.ty.gql_name());
            }
        }
        Ok(Schema { nodes, edges, values: BTreeMap::new() })
    }

    /// The declared schema with random node types, edge types, and properties added, and every type's count and
    /// density drawn, all from the seed; the same seed gives the same schema. Without `random`, the schema itself.
    pub fn randomized(&self, seed: u64) -> SchemaConfig {
        let Some(random) = &self.random else { return self.clone() };
        let mut rng = Rng::new(seed ^ 0x5c4e_3a00_0000_0002);
        let draw = |range: [i64; 2], rng: &mut Rng| rng.range(range[0].min(range[1]), range[0].max(range[1]));
        let mut result = SchemaConfig { nodes: self.nodes.clone(), edges: self.edges.clone(), random: None };
        // Every property name keeps one type: the declared ones as declared, new ones drawn once.
        let mut types: BTreeMap<String, String> = BTreeMap::new();
        for properties in result.nodes.values().map(|node| &node.properties).chain(result.edges.values().map(|edge| &edge.properties)) {
            for (name, ty) in properties {
                types.entry(name.clone()).or_insert_with(|| ty.to_uppercase().trim_end_matches("NOT NULL").trim().to_string());
            }
        }
        const TYPES: [&str; 8] = ["INT64", "FLOAT64", "STRING", "BOOL", "DATE", "LOCAL DATETIME", "ZONED DATETIME", "DECIMAL"];
        for name in ["rank", "level", "rating", "price", "amount", "title", "code", "flag", "created", "updated", "tier", "region"] {
            types.entry(name.to_string()).or_insert_with(|| rng.pick(&TYPES).to_string());
        }
        let names: Vec<String> = types.keys().filter(|name| name.as_str() != "id").cloned().collect();
        let add_properties = |properties: &mut BTreeMap<String, String>, rng: &mut Rng| {
            let extra = draw([random.properties[0] as i64, random.properties[1] as i64], rng);
            for _ in 0..extra {
                let name = rng.pick(&names).clone();
                let ty = types[&name].clone();
                properties.entry(name).or_insert(ty);
            }
        };
        let node_names = ["Account", "Item", "Device", "Post", "Forum", "Movie", "Topic", "Team", "Venue", "Region"];
        let added = draw([random.node_types[0] as i64, random.node_types[1] as i64], &mut rng);
        let fresh: Vec<&str> = node_names.iter().copied().filter(|name| !result.nodes.contains_key(*name)).take(added as usize).collect();
        for name in fresh {
            let mut properties = BTreeMap::from([("id".to_string(), "INT64 NOT NULL".to_string())]);
            add_properties(&mut properties, &mut rng);
            result.nodes.insert(name.to_string(), NodeConfig { count: 1, properties });
        }
        for node in result.nodes.values_mut() {
            node.count = draw(random.nodes, &mut rng).max(1);
            add_properties(&mut node.properties, &mut rng);
        }
        let labels: Vec<String> = result.nodes.keys().cloned().collect();
        let edge_names = ["OWNS", "LIKES", "FOLLOWS", "BOUGHT", "TAGGED", "MEMBER_OF", "LINKS", "REPLIES_TO", "USES", "HOSTS"];
        let added = draw([random.edge_types[0] as i64, random.edge_types[1] as i64], &mut rng);
        let fresh: Vec<&str> = edge_names.iter().copied().filter(|name| !result.edges.contains_key(*name)).take(added as usize).collect();
        for (index, name) in fresh.into_iter().enumerate() {
            let from = rng.pick(&labels).clone();
            // The first is a self-loop type, so quantified paths always have one to repeat over.
            let to = if index == 0 { from.clone() } else { rng.pick(&labels).clone() };
            let mut properties = BTreeMap::new();
            add_properties(&mut properties, &mut rng);
            result.edges.insert(name.to_string(), EdgeConfig { from, to, undirected: false, properties, count: None });
        }
        for edge in result.edges.values_mut() {
            let sources = result.nodes[&edge.from].count;
            edge.count = Some((sources * draw(random.degree, &mut rng)).clamp(1, 60));
            if edge.properties.len() < 3 {
                add_properties(&mut edge.properties, &mut rng);
            }
        }
        result
    }
}

pub struct FixtureNode {
    pub label: &'static str,
    pub props: Vec<(&'static str, Lit)>,
}

pub struct FixtureEdge {
    pub label: &'static str,
    pub source: (&'static str, i64),
    pub target: (&'static str, i64),
    pub undirected: bool,
    pub props: Vec<(&'static str, Lit)>,
}

pub struct Fixture {
    pub nodes: Vec<FixtureNode>,
    pub edges: Vec<FixtureEdge>,
}

impl Schema {
    pub fn node(&self, label: &str) -> &NodeType {
        self.nodes.iter().find(|node| node.label == label).expect("unknown node label")
    }

    pub fn edge(&self, label: &str) -> &EdgeType {
        self.edges.iter().find(|edge| edge.label == label).expect("unknown edge label")
    }

    /// The schema with the values the seed's fixture holds, for generators that compare with data.
    pub fn with_values(mut self, seed: u64) -> Schema {
        let fixture = self.fixture(seed);
        let mut values: BTreeMap<(&'static str, &'static str), Vec<Lit>> = BTreeMap::new();
        for (label, props) in fixture.nodes.iter().map(|node| (node.label, &node.props)).chain(fixture.edges.iter().map(|edge| (edge.label, &edge.props))) {
            for (name, value) in props {
                values.entry((label, *name)).or_default().push(value.clone());
            }
        }
        self.values = values;
        self
    }

    /// A short description, one line per type, for the report.
    pub fn describe(&self) -> String {
        let props = |props: &[Prop]| props.iter().map(|prop| format!("{} {}", prop.name, prop.ty.gql_name())).collect::<Vec<_>>().join(", ");
        let mut lines: Vec<String> = self.nodes.iter().map(|node| format!("- ({}) x{}: {}", node.label, node.count, props(&node.props))).collect();
        lines.extend(self.edges.iter().map(|edge| {
            let arrow = if edge.undirected { "~" } else { "->" };
            format!("- ({})-[{}]{}({}) x{}: {}", edge.source, edge.label, arrow, edge.target, edge.count, props(&edge.props))
        }));
        lines.join("\n")
    }

    /// The property types any node or edge has, so the generator writes values of those types.
    pub fn types(&self) -> Vec<Ty> {
        let mut types: Vec<Ty> = Vec::new();
        for prop in self.nodes.iter().flat_map(|node| node.props.iter()).chain(self.edges.iter().flat_map(|edge| edge.props.iter())) {
            if !types.contains(&prop.ty) {
                types.push(prop.ty);
            }
        }
        types
    }

    /// The GQL statement that declares the graph type.
    pub fn gql_create_graph(&self, name: &str) -> String {
        let mut elements = Vec::new();
        for node in &self.nodes {
            let props: Vec<String> =
                node.props.iter().map(|prop| format!("{} {}{}", prop.name, prop.ty.gql_name(), if prop.required { " NOT NULL" } else { "" })).collect();
            elements.push(format!("({} :{} {{{}}})", node.label, node.label, props.join(", ")));
        }
        for edge in &self.edges {
            let props: Vec<String> = edge.props.iter().map(|prop| format!("{} {}", prop.name, prop.ty.gql_name())).collect();
            let filler = if props.is_empty() { format!(":{}", edge.label) } else { format!(":{} {{{}}}", edge.label, props.join(", ")) };
            if edge.undirected {
                elements.push(format!("({})~[{}]~({})", edge.source, filler, edge.target));
            } else {
                elements.push(format!("({})-[{}]->({})", edge.source, filler, edge.target));
            }
        }
        format!("CREATE GRAPH {} {{ {} }}", name, elements.join(", "))
    }

    /// Fixture data, the same for a seed: optional properties are sometimes absent, so queries meet NULLs.
    pub fn fixture(&self, seed: u64) -> Fixture {
        let mut rng = Rng::new(seed ^ 0x5eed_f1c7_0000_0001);
        let names = ["Ada", "Bob", "Cy", "Dee", "Eve", "Fay", "Gus", "Hal", "Ivy", "Jo", "Kim", "Lu"];
        let mut fixture = Fixture { nodes: Vec::new(), edges: Vec::new() };
        for node in &self.nodes {
            for id in 1..=node.count {
                let mut props = Vec::new();
                for prop in &node.props {
                    if prop.name == "id" {
                        props.push((prop.name, Lit::Int(id)));
                        continue;
                    }
                    if !prop.required && rng.chance(0.2) {
                        continue;
                    }
                    let value = match (prop.name, prop.ty) {
                        ("name", Ty::Str) if node.label == "Person" => Lit::Str(names[(id as usize - 1) % names.len()].to_string()),
                        ("name", Ty::Str) => Lit::Str(format!("{}{id}", node.label)),
                        (_, ty) => ty.random(&mut rng),
                    };
                    props.push((prop.name, value));
                }
                fixture.nodes.push(FixtureNode { label: node.label, props });
            }
        }
        let count_of = |label: &str| self.node(label).count;
        for edge in &self.edges {
            for _ in 0..edge.count {
                let source = rng.range(1, count_of(edge.source));
                let target = rng.range(1, count_of(edge.target));
                let mut props = Vec::new();
                for prop in &edge.props {
                    if prop.required || !rng.chance(0.25) {
                        let value = match prop.ty {
                            Ty::Int => Lit::Int(rng.range(1, 30)),
                            Ty::Float => Lit::Float(rng.range(0, 80) as f64 / 8.0),
                            Ty::Str => Lit::Str(format!("r{}", rng.range(0, 3))),
                            ty => ty.random(&mut rng),
                        };
                        props.push((prop.name, value));
                    }
                }
                fixture.edges.push(FixtureEdge {
                    label: edge.label,
                    source: (edge.source, source),
                    target: (edge.target, target),
                    undirected: edge.undirected,
                    props,
                });
            }
        }
        fixture
    }
}

impl Fixture {
    /// The statements that load the fixture, as ASTs; `keep` decides which property values they write.
    pub fn statements(&self, keep: impl Fn(&Lit) -> bool) -> Vec<Query> {
        let map = |props: &[(&'static str, Lit)]| -> Vec<(String, Expr)> {
            props.iter().filter(|(_, value)| keep(value)).map(|(name, value)| (name.to_string(), Expr::Lit(value.clone()))).collect()
        };
        let mut statements = Vec::new();
        for node in &self.nodes {
            let start = NodePat { var: None, label: Some(LabelExpr::Name(node.label.to_string())), props: map(&node.props), filter: None };
            statements.push(Query::Linear(Linear {
                clauses: vec![Clause::Insert(vec![PathPat { var: None, search: None, mode: None, start, hops: vec![] }])],
                tail: Tail::None,
            }));
        }
        let id = |var: &str, value: i64| Expr::Bin(BinOp::Eq, Box::new(Expr::Prop(var.to_string(), "id".to_string())), Box::new(Expr::Lit(Lit::Int(value))));
        for edge in &self.edges {
            let endpoint =
                |var: &str, label: &str| NodePat { var: Some(var.to_string()), label: Some(LabelExpr::Name(label.to_string())), props: vec![], filter: None };
            let bound = |var: &str| NodePat { var: Some(var.to_string()), label: None, props: vec![], filter: None };
            let arc = EdgePat {
                var: None,
                label: Some(LabelExpr::Name(edge.label.to_string())),
                dir: if edge.undirected { Dir::Undirected } else { Dir::Right },
                quant: None,
                props: map(&edge.props),
                filter: None,
            };
            statements.push(Query::Linear(Linear {
                clauses: vec![
                    Clause::Match {
                        optional: false,
                        mode: None,
                        paths: vec![
                            PathPat { var: None, search: None, mode: None, start: endpoint("a", edge.source.0), hops: vec![] },
                            PathPat { var: None, search: None, mode: None, start: endpoint("b", edge.target.0), hops: vec![] },
                        ],
                        filter: Some(Expr::Bin(BinOp::And, Box::new(id("a", edge.source.1)), Box::new(id("b", edge.target.1)))),
                    },
                    Clause::Insert(vec![PathPat { var: None, search: None, mode: None, start: bound("a"), hops: vec![(arc, bound("b"))] }]),
                ],
                tail: Tail::None,
            }));
        }
        statements
    }
}
