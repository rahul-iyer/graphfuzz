//! Dialect profiles: which features a query language has and how it spells them. The built-in profiles are
//! the YAML files in `profiles/`; an engine names one and may adjust it, so a new endpoint needs no code.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::generate::FEATURES;

const BUILT_IN: &[(&str, &str)] = &[
    ("gql", include_str!("../profiles/gql.yml")),
    ("cypher25", include_str!("../profiles/cypher25.yml")),
    ("opencypher", include_str!("../profiles/opencypher.yml")),
];

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Quantifier {
    /// -[:K]->{1,3}
    Braces,
    /// -[:K*1..3]->
    Star,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Syntax {
    pub quantifier: Quantifier,
    pub concat: String,
    /// Whether an approximate number literal needs an exponent, as in GQL.
    pub float_exponent: bool,
    /// Whether ORDER BY says where NULLs go; otherwise they follow Neo4j: last going up, first going down.
    pub explicit_nulls: bool,
    pub insert: String,
    pub offset: String,
    /// LET, FILTER, NEXT, and paging before RETURN are written with WITH.
    pub linear_as_with: bool,
    /// What follows CALL for a call that imports every variable.
    pub call_all: String,
    pub different_edges: String,
    /// The language's sum of no values is 0, where GQL's is NULL.
    pub sum_empty_is_zero: bool,
    /// The NULLIF function's name, or `case` to write it as a CASE expression.
    pub nullif: String,
    /// `function` for PROPERTY_EXISTS(n, p), `is_not_null` for n.p IS NOT NULL.
    pub property_exists: String,
    /// `gql` for CAST(x AS T), `functions` for toString(x) and the like.
    pub cast: String,
    /// `gql` for n IS LABELED A, `colon` for n:A.
    pub labeled: String,
    /// Function names that differ from GQL's.
    pub functions: BTreeMap<String, String>,
    /// GQL functions the language writes as binary operators, as MOD(a, b) is a % b.
    pub operators: BTreeMap<String, String>,
    /// How to write a value of each type, with {} for the value: date, local_datetime, zoned_datetime, duration.
    #[serde(default)]
    pub literals: BTreeMap<String, String>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub name: String,
    /// Text put before every query, as CYPHER 25 for Neo4j.
    pub prefix: String,
    pub unsupported: Vec<String>,
    pub syntax: Syntax,
}

/// Merges `overrides` into `base`: maps key by key, everything else replaced.
fn merge(base: &mut serde_yaml::Value, overrides: &serde_yaml::Value) {
    match (base, overrides) {
        (serde_yaml::Value::Mapping(base), serde_yaml::Value::Mapping(overrides)) => {
            for (key, value) in overrides {
                match base.get_mut(key) {
                    Some(existing) => merge(existing, value),
                    None => {
                        base.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (base, overrides) => *base = overrides.clone(),
    }
}

impl Profile {
    /// A built-in profile by name, or a profile file, adjusted for one engine.
    pub fn load(dialect: &str, base_dir: &Path, supported: &[String], unsupported: &[String], syntax: Option<&serde_yaml::Value>) -> Result<Profile> {
        let text = match BUILT_IN.iter().find(|(name, _)| *name == dialect) {
            Some((_, text)) => text.to_string(),
            None => {
                let path = base_dir.join(dialect);
                std::fs::read_to_string(&path).with_context(|| {
                    let names: Vec<&str> = BUILT_IN.iter().map(|(name, _)| *name).collect();
                    format!("dialect '{dialect}' is neither a built-in profile ({}) nor a readable file", names.join(", "))
                })?
            }
        };
        let mut value: serde_yaml::Value = serde_yaml::from_str(&text).with_context(|| format!("parsing dialect {dialect}"))?;
        if let Some(syntax) = syntax {
            let mut overrides = serde_yaml::Mapping::new();
            overrides.insert("syntax".into(), syntax.clone());
            merge(&mut value, &serde_yaml::Value::Mapping(overrides));
        }
        let mut profile: Profile = serde_yaml::from_value(value).with_context(|| format!("reading dialect {dialect}"))?;
        let mut features: BTreeSet<String> = profile.unsupported.iter().cloned().collect();
        for feature in supported {
            features.remove(feature);
        }
        features.extend(unsupported.iter().cloned());
        for feature in &features {
            if !FEATURES.iter().any(|(name, _)| name == feature) {
                bail!("dialect {dialect}: unknown feature '{feature}'");
            }
        }
        profile.unsupported = features.into_iter().collect();
        Ok(profile)
    }

    /// The features of a query this language cannot express.
    pub fn missing(&self, features: &BTreeSet<&'static str>) -> Vec<&'static str> {
        features.iter().copied().filter(|feature| self.unsupported.iter().any(|name| name == feature)).collect()
    }

    pub fn function<'a>(&'a self, name: &'a str) -> &'a str {
        self.syntax.functions.get(name).map(String::as_str).unwrap_or(name)
    }
}
