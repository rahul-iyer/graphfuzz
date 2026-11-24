//! The run configuration, read from a YAML file. Every field has a default, so a file need only name
//! what it changes; `config.yml` in the repository documents them all.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::generate::{FEATURES, GROUPS};

#[derive(Deserialize, Debug, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct FileConfig {
    pub seed: u64,
    /// Where the report and the per-case results go, relative to the config file.
    pub out: PathBuf,
    pub timeout_ms: u64,
    pub neo4j: Neo4jConfig,
    pub phases: Phases,
    pub generator: GeneratorConfig,
    pub checks: Checks,
    /// The file as written, for the report.
    #[serde(skip)]
    pub raw: String,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct Neo4jConfig {
    pub enabled: bool,
    pub url: String,
    pub user: String,
    pub password: String,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct Phases {
    /// Queries per feature with only that feature on.
    pub isolate: usize,
    /// The features isolated; empty for all.
    pub isolate_features: Vec<String>,
    /// Random queries both engines must answer the same.
    pub compare: usize,
    /// Random queries that may use GQL-only features, checked on DuckGQL alone.
    pub explore: usize,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct GeneratorConfig {
    /// The share of random queries that modify data.
    pub mutation_share: f64,
    /// Multiplies how often a feature, or every feature of a group, is used. 0 turns it off.
    pub weights: BTreeMap<String, f64>,
    /// Features or groups used wherever the generator could use them.
    pub focus: Vec<String>,
    /// Features or groups never used.
    pub disable: Vec<String>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct Checks {
    /// Run the rewrites that must not change an answer: partitioning, path reversal, count, DISTINCT.
    pub metamorphic: bool,
    /// Shrink one failing case per failure group to a minimal query.
    pub reduce: bool,
    /// Variants tried per reduction.
    pub reduce_budget: usize,
}

impl Default for FileConfig {
    fn default() -> Self {
        FileConfig {
            seed: 1,
            out: PathBuf::from("runs/latest"),
            timeout_ms: 10_000,
            neo4j: Neo4jConfig::default(),
            phases: Phases::default(),
            generator: GeneratorConfig::default(),
            checks: Checks::default(),
            raw: String::new(),
        }
    }
}

impl Default for Neo4jConfig {
    fn default() -> Self {
        Neo4jConfig {
            enabled: true,
            url: "http://localhost:7474".to_string(),
            user: "neo4j".to_string(),
            password: "graphfuzz123".to_string(),
        }
    }
}

impl Default for Phases {
    fn default() -> Self {
        Phases { isolate: 6, isolate_features: Vec::new(), compare: 1000, explore: 400 }
    }
}

impl Default for GeneratorConfig {
    fn default() -> Self {
        GeneratorConfig { mutation_share: 0.15, weights: BTreeMap::new(), focus: Vec::new(), disable: Vec::new() }
    }
}

impl Default for Checks {
    fn default() -> Self {
        Checks { metamorphic: true, reduce: true, reduce_budget: 150 }
    }
}

/// How the generator weighs features, with group names expanded to their features.
#[derive(Clone, Debug, Default)]
pub struct Weights {
    pub multipliers: BTreeMap<&'static str, f64>,
    pub focus: BTreeSet<&'static str>,
    pub disabled: BTreeSet<&'static str>,
    pub mutation_share: f64,
}

impl Weights {
    pub fn multiplier(&self, feature: &str) -> f64 {
        self.multipliers.get(feature).copied().unwrap_or(1.0)
    }
}

/// The features a feature or group name stands for.
fn expand(name: &str) -> Result<Vec<&'static str>> {
    if let Some((feature, _)) = FEATURES.iter().find(|(feature, _)| *feature == name) {
        return Ok(vec![*feature]);
    }
    if let Some((_, members)) = GROUPS.iter().find(|(group, _)| *group == name) {
        return Ok(members.to_vec());
    }
    let groups: Vec<&str> = GROUPS.iter().map(|(group, _)| *group).collect();
    bail!("unknown feature or group '{name}'; groups are {}, and `graphfuzz features` lists the features", groups.join(", "))
}

impl FileConfig {
    pub fn load(path: Option<&Path>) -> Result<FileConfig> {
        let Some(path) = path else { return Ok(FileConfig::default()) };
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut config: FileConfig = serde_yaml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        config.raw = text;
        if config.out.is_relative() {
            config.out = path.parent().unwrap_or(Path::new(".")).join(&config.out);
        }
        config.weights()?;
        for feature in &config.phases.isolate_features {
            expand(feature)?;
        }
        if !(0.0..=1.0).contains(&config.generator.mutation_share) {
            bail!("generator.mutation_share must be between 0 and 1");
        }
        Ok(config)
    }

    /// The weights, focus, and disabled features, with groups expanded; a feature named directly wins over its group.
    pub fn weights(&self) -> Result<Weights> {
        let mut weights = Weights { mutation_share: self.generator.mutation_share, ..Weights::default() };
        let mut direct = BTreeSet::new();
        // Groups first, so a feature's own weight overrides its group's.
        let mut entries: Vec<(&String, &f64)> = self.generator.weights.iter().collect();
        entries.sort_by_key(|(name, _)| FEATURES.iter().any(|(feature, _)| *feature == name.as_str()));
        for (name, multiplier) in entries {
            if *multiplier < 0.0 {
                bail!("the weight of '{name}' must not be negative");
            }
            let is_feature = FEATURES.iter().any(|(feature, _)| *feature == name.as_str());
            for feature in expand(name)? {
                if is_feature || !direct.contains(feature) {
                    weights.multipliers.insert(feature, *multiplier);
                }
                if is_feature {
                    direct.insert(feature);
                }
            }
        }
        for name in &self.generator.focus {
            weights.focus.extend(expand(name)?);
        }
        for name in &self.generator.disable {
            weights.disabled.extend(expand(name)?);
        }
        if let Some(both) = weights.focus.intersection(&weights.disabled).next() {
            bail!("'{both}' is both focused and disabled");
        }
        Ok(weights)
    }

    /// The features the isolation phase runs, in the order the generator lists them.
    pub fn isolated_features(&self) -> Result<Vec<&'static str>> {
        if self.phases.isolate_features.is_empty() {
            return Ok(FEATURES.iter().map(|(feature, _)| *feature).collect());
        }
        let mut chosen = BTreeSet::new();
        for name in &self.phases.isolate_features {
            chosen.extend(expand(name)?);
        }
        Ok(FEATURES.iter().map(|(feature, _)| *feature).filter(|feature| chosen.contains(feature)).collect())
    }
}
