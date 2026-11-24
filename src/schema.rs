//! The graph the generator writes queries for, and the fixture data both engines load.

use crate::rng::Rng;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    Int,
    Float,
    Str,
    Bool,
}

impl Ty {
    pub fn gql_name(self) -> &'static str {
        match self {
            Ty::Int => "INT64",
            Ty::Float => "FLOAT64",
            Ty::Str => "STRING",
            Ty::Bool => "BOOL",
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
}

pub struct EdgeType {
    pub label: &'static str,
    pub source: &'static str,
    pub target: &'static str,
    pub undirected: bool,
    pub props: Vec<Prop>,
}

pub struct Schema {
    pub nodes: Vec<NodeType>,
    pub edges: Vec<EdgeType>,
}

fn prop(name: &'static str, ty: Ty, required: bool) -> Prop {
    Prop { name, ty, required }
}

/// A property value in the fixture.
#[derive(Clone, Debug)]
pub enum FixtureValue {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
}

impl FixtureValue {
    fn literal(&self, gql: bool) -> String {
        match self {
            FixtureValue::Int(value) => value.to_string(),
            // GQL reads 1.5 as an exact number; the exponent makes it approximate, as Cypher's 1.5 is.
            FixtureValue::Float(value) => {
                if gql {
                    format!("{value:?}E0")
                } else {
                    format!("{value:?}")
                }
            }
            FixtureValue::Str(value) => format!("'{}'", value.replace('\'', "''")),
            FixtureValue::Bool(value) => value.to_string(),
        }
    }
}

pub struct FixtureNode {
    pub label: &'static str,
    pub id: i64,
    pub props: Vec<(&'static str, FixtureValue)>,
}

pub struct FixtureEdge {
    pub label: &'static str,
    pub source: (&'static str, i64),
    pub target: (&'static str, i64),
    pub undirected: bool,
    pub props: Vec<(&'static str, FixtureValue)>,
}

pub struct Fixture {
    pub seed: u64,
    pub nodes: Vec<FixtureNode>,
    pub edges: Vec<FixtureEdge>,
}

fn properties(props: &[(&'static str, FixtureValue)], gql: bool) -> String {
    if props.is_empty() {
        return String::new();
    }
    let pairs: Vec<String> = props.iter().map(|(name, value)| format!("{}: {}", name, value.literal(gql))).collect();
    format!(" {{{}}}", pairs.join(", "))
}

impl Schema {
    /// A social graph. Without undirected edges it means the same in GQL and in Neo4j, whose
    /// relationships are all directed.
    pub fn social(undirected: bool) -> Schema {
        let mut edges = vec![
            EdgeType {
                label: "KNOWS",
                source: "Person",
                target: "Person",
                undirected: false,
                props: vec![prop("since", Ty::Int, false), prop("weight", Ty::Float, false)],
            },
            EdgeType { label: "LIVES_IN", source: "Person", target: "City", undirected: false, props: vec![prop("since", Ty::Int, false)] },
            EdgeType {
                label: "WORKS_AT",
                source: "Person",
                target: "Company",
                undirected: false,
                props: vec![prop("since", Ty::Int, false), prop("role", Ty::Str, false)],
            },
            EdgeType { label: "LOCATED_IN", source: "Company", target: "City", undirected: false, props: vec![] },
        ];
        if undirected {
            edges.push(EdgeType { label: "NEAR", source: "City", target: "City", undirected: true, props: vec![prop("km", Ty::Int, false)] });
        }
        Schema {
            nodes: vec![
                NodeType {
                    label: "Person",
                    props: vec![
                        prop("id", Ty::Int, true),
                        prop("name", Ty::Str, false),
                        prop("age", Ty::Int, false),
                        prop("score", Ty::Float, false),
                        prop("active", Ty::Bool, false),
                    ],
                },
                NodeType {
                    label: "City",
                    props: vec![prop("id", Ty::Int, true), prop("name", Ty::Str, false), prop("population", Ty::Int, false)],
                },
                NodeType {
                    label: "Company",
                    props: vec![prop("id", Ty::Int, true), prop("name", Ty::Str, false), prop("founded", Ty::Int, false)],
                },
            ],
            edges,
        }
    }

    pub fn node(&self, label: &str) -> &NodeType {
        self.nodes.iter().find(|node| node.label == label).expect("unknown node label")
    }

    pub fn edge(&self, label: &str) -> &EdgeType {
        self.edges.iter().find(|edge| edge.label == label).expect("unknown edge label")
    }

    pub fn has_undirected(&self) -> bool {
        self.edges.iter().any(|edge| edge.undirected)
    }

    /// The GQL statement that declares the graph type.
    pub fn gql_create_graph(&self, name: &str) -> String {
        let mut elements = Vec::new();
        for node in &self.nodes {
            let props: Vec<String> = node
                .props
                .iter()
                .map(|prop| format!("{} {}{}", prop.name, prop.ty.gql_name(), if prop.required { " NOT NULL" } else { "" }))
                .collect();
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
        let counts = [("Person", 12i64), ("City", 5), ("Company", 4)];
        let mut fixture = Fixture { seed, nodes: Vec::new(), edges: Vec::new() };
        for (label, count) in counts {
            for id in 1..=count {
                let mut props = vec![("id", FixtureValue::Int(id))];
                for prop in &self.node(label).props {
                    if prop.required || rng.chance(0.2) {
                        continue;
                    }
                    let value = match (prop.name, prop.ty) {
                        ("name", _) if label == "Person" => FixtureValue::Str(format!("{}", names[(id as usize - 1) % names.len()])),
                        ("name", _) => FixtureValue::Str(format!("{label}{id}")),
                        (_, Ty::Int) => FixtureValue::Int(rng.range(1, 60)),
                        (_, Ty::Float) => FixtureValue::Float(rng.range(0, 400) as f64 / 8.0),
                        (_, Ty::Bool) => FixtureValue::Bool(rng.chance(0.5)),
                        (_, Ty::Str) => FixtureValue::Str(format!("v{}", rng.range(0, 5))),
                    };
                    props.push((prop.name, value));
                }
                fixture.nodes.push(FixtureNode { label, id, props });
            }
        }
        let count_of = |label: &str| counts.iter().find(|(name, _)| *name == label).unwrap().1;
        for edge in &self.edges {
            let edge_count = (count_of(edge.source) * 2).min(20);
            for _ in 0..edge_count {
                let source = rng.range(1, count_of(edge.source));
                let target = rng.range(1, count_of(edge.target));
                let mut props = Vec::new();
                for prop in &edge.props {
                    if rng.chance(0.25) {
                        continue;
                    }
                    let value = match prop.ty {
                        Ty::Int => FixtureValue::Int(rng.range(1, 30)),
                        Ty::Float => FixtureValue::Float(rng.range(0, 80) as f64 / 8.0),
                        Ty::Str => FixtureValue::Str(format!("r{}", rng.range(0, 3))),
                        Ty::Bool => FixtureValue::Bool(true),
                    };
                    props.push((prop.name, value));
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
    pub fn gql_statements(&self) -> Vec<String> {
        let mut statements = Vec::new();
        for node in &self.nodes {
            statements.push(format!("INSERT (:{}{})", node.label, properties(&node.props, true)));
        }
        for edge in &self.edges {
            let filler = format!(":{}{}", edge.label, properties(&edge.props, true));
            let arc = if edge.undirected { format!("~[{filler}]~") } else { format!("-[{filler}]->") };
            statements.push(format!(
                "MATCH (a:{}), (b:{}) WHERE a.id = {} AND b.id = {} INSERT (a){}(b)",
                edge.source.0, edge.target.0, edge.source.1, edge.target.1, arc
            ));
        }
        statements
    }

    pub fn cypher_statements(&self) -> Vec<String> {
        let mut statements = vec!["MATCH (n) DETACH DELETE n".to_string()];
        for node in &self.nodes {
            statements.push(format!("CREATE (:{}{})", node.label, properties(&node.props, false)));
        }
        for edge in &self.edges {
            statements.push(format!(
                "MATCH (a:{} {{id: {}}}), (b:{} {{id: {}}}) CREATE (a)-[:{}{}]->(b)",
                edge.source.0,
                edge.source.1,
                edge.target.0,
                edge.target.1,
                edge.label,
                properties(&edge.props, false)
            ));
        }
        statements
    }
}
