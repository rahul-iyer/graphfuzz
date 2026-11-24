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

    pub fn from_json(value: &serde_json::Value) -> Val {
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
            D::Struct(fields) => Val::Map(fields.iter().map(|(name, value)| (name.clone(), Val::from_duckdb(value.clone()))).collect()),
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
