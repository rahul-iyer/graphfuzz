//! Result values from either engine, normalized so equal answers compare equal: every number is a
//! double, and lists compare as multisets because aggregation order is unspecified.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Val {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    List(Vec<Val>),
    Map(Vec<(String, Val)>),
    Other(String),
}

impl Val {
    /// A canonical text for sorting and comparison. Numbers agree to seven significant digits, because Neo4j's
    /// JSON rounds doubles.
    pub fn key(&self) -> String {
        match self {
            Val::Null => "null".to_string(),
            Val::Bool(value) => value.to_string(),
            Val::Num(value) => {
                if value.is_nan() {
                    "nan".to_string()
                } else if value.fract() == 0.0 && value.abs() < 1e15 {
                    format!("{}", *value as i64)
                } else {
                    format!("{:.6e}", value)
                }
            }
            Val::Str(value) => format!("{value:?}"),
            Val::List(items) => {
                let mut keys: Vec<String> = items.iter().map(Val::key).collect();
                keys.sort();
                format!("[{}]", keys.join(","))
            }
            Val::Map(fields) => {
                let mut keys: Vec<String> = fields.iter().map(|(name, value)| format!("{name}:{}", value.key())).collect();
                keys.sort();
                format!("{{{}}}", keys.join(","))
            }
            Val::Other(text) => format!("<{text}>"),
        }
    }

    /// A node as engines agree on it: its labels and the properties it has, without engine-internal ids.
    pub fn node(labels: Vec<String>, properties: Vec<(String, Val)>) -> Val {
        let mut labels: Vec<Val> = labels.into_iter().map(|label| Val::Str(label.to_lowercase())).collect();
        labels.sort_by_key(Val::key);
        let properties = properties.into_iter().filter(|(_, value)| *value != Val::Null).collect();
        Val::Map(vec![("labels".to_string(), Val::List(labels)), ("properties".to_string(), Val::Map(properties))])
    }

    pub fn relationship(label: &str, properties: Vec<(String, Val)>) -> Val {
        let properties = properties.into_iter().filter(|(_, value)| *value != Val::Null).collect();
        Val::Map(vec![("type".to_string(), Val::Str(label.to_lowercase())), ("properties".to_string(), Val::Map(properties))])
    }

    /// A path by its relationships: engines disagree on whether a path value lists its end nodes.
    pub fn path(relationships: Vec<Val>) -> Val {
        Val::Map(vec![("path".to_string(), Val::List(relationships))])
    }

    fn json_element(fields: &serde_json::Map<String, serde_json::Value>) -> Option<Val> {
        fields.get("elementId")?;
        let properties = fields
            .get("properties")
            .and_then(|properties| properties.as_object())
            .map(|properties| properties.iter().map(|(name, value)| (name.clone(), Val::from_json(value))).collect())
            .unwrap_or_default();
        if let Some(labels) = fields.get("labels").and_then(|labels| labels.as_array()) {
            return Some(Val::node(labels.iter().filter_map(|label| label.as_str().map(String::from)).collect(), properties));
        }
        let label = fields.get("type").and_then(|label| label.as_str())?;
        Some(Val::relationship(label, properties))
    }

    pub fn from_json(value: &serde_json::Value) -> Val {
        // Neo4j writes nodes and relationships as objects with an elementId, and a path as a list of them.
        if let serde_json::Value::Object(fields) = value {
            if let Some(element) = Val::json_element(fields) {
                return element;
            }
        }
        if let serde_json::Value::Array(items) = value {
            if !items.is_empty() && items.iter().all(|item| item.get("elementId").is_some()) {
                let relationships = items.iter().filter(|item| item.get("type").is_some()).map(Val::from_json).collect();
                return Val::path(relationships);
            }
        }
        match value {
            serde_json::Value::Null => Val::Null,
            serde_json::Value::Bool(value) => Val::Bool(*value),
            serde_json::Value::Number(number) => Val::Num(number.as_f64().unwrap_or(f64::NAN)),
            serde_json::Value::String(text) => Val::Str(text.clone()),
            serde_json::Value::Array(items) => Val::List(items.iter().map(Val::from_json).collect()),
            serde_json::Value::Object(fields) => Val::Map(fields.iter().map(|(name, value)| (name.clone(), Val::from_json(value))).collect()),
        }
    }

    pub fn from_duckdb(value: duckdb::types::Value) -> Val {
        use duckdb::types::Value as D;
        match value {
            D::Null => Val::Null,
            D::Boolean(value) => Val::Bool(value),
            D::TinyInt(value) => Val::Num(value as f64),
            D::SmallInt(value) => Val::Num(value as f64),
            D::Int(value) => Val::Num(value as f64),
            D::BigInt(value) => Val::Num(value as f64),
            D::HugeInt(value) => Val::Num(value as f64),
            D::UTinyInt(value) => Val::Num(value as f64),
            D::USmallInt(value) => Val::Num(value as f64),
            D::UInt(value) => Val::Num(value as f64),
            D::UBigInt(value) => Val::Num(value as f64),
            D::Float(value) => Val::Num(value as f64),
            D::Double(value) => Val::Num(value),
            D::Decimal(value) => Val::Num(value.to_string().parse().unwrap_or(f64::NAN)),
            D::Text(text) => Val::Str(text),
            D::List(items) | D::Array(items) => Val::List(items.into_iter().map(Val::from_duckdb).collect()),
            D::Struct(fields) => {
                let fields: Vec<(String, Val)> = fields.iter().map(|(name, value)| (name.clone(), Val::from_duckdb(value.clone()))).collect();
                // DuckGQL returns a node or an edge as a struct with its id, labels, and properties.
                let get = |key: &str| fields.iter().find(|(name, _)| name == key).map(|(_, value)| value.clone());
                let properties = || {
                    fields.iter().filter(|(name, _)| !name.starts_with("__gql") && name != "vertex_id" && name != "edge_id").cloned().collect::<Vec<_>>()
                };
                if get("vertex_id").is_some() {
                    let labels = match get("__gql_labels") {
                        Some(Val::Str(labels)) => labels.split(';').filter(|label| !label.is_empty()).map(String::from).collect(),
                        Some(Val::List(labels)) => labels.iter().filter_map(|label| if let Val::Str(label) = label { Some(label.clone()) } else { None }).collect(),
                        _ => Vec::new(),
                    };
                    Val::node(labels, properties())
                } else if get("edge_id").is_some() {
                    let label = match get("__gql_type") {
                        Some(Val::Str(label)) => label,
                        _ => String::new(),
                    };
                    Val::relationship(&label, properties())
                } else {
                    Val::Map(fields)
                }
            }
            other => Val::Other(format!("{other:?}")),
        }
    }
}

pub type Row = Vec<Val>;

/// Rows as a sorted list of keys, so two results compare as multisets.
pub fn multiset(rows: &[Row]) -> Vec<String> {
    let mut keys: Vec<String> = rows.iter().map(|row| row.iter().map(Val::key).collect::<Vec<_>>().join(" | ")).collect();
    keys.sort();
    keys
}

pub fn sequence(rows: &[Row]) -> Vec<String> {
    rows.iter().map(|row| row.iter().map(Val::key).collect::<Vec<_>>().join(" | ")).collect()
}
