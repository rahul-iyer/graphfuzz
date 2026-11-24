//! Engines under test and the reference engine, behind one interface. A connector knows how to reach an
//! engine and load the fixture into it; the engine's dialect profile says how to write queries for it.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;

use crate::neo::{Neo, TRANSPORT_ERROR};
use crate::schema::{Fixture, Schema, Ty};
use crate::value::{Row, Val};
use crate::worker::{RunOutcome, Statement, StatementResult, Worker};

/// What one statement did.
#[derive(Clone, Debug)]
pub enum Outcome {
    Rows { columns: Vec<String>, rows: Vec<Row> },
    Done,
    Error(String),
    Crash(String),
    Timeout,
    /// The engine could not be reached, or its answer read.
    Transport(String),
}

impl Outcome {
    pub fn rows(&self) -> Option<&Vec<Row>> {
        match self {
            Outcome::Rows { rows, .. } => Some(rows),
            _ => None,
        }
    }

    fn from_statement(result: StatementResult) -> Outcome {
        match result {
            StatementResult::Rows { columns, rows } => Outcome::Rows { columns, rows },
            StatementResult::Done => Outcome::Done,
            StatementResult::Error(message) if message.starts_with(TRANSPORT_ERROR) => Outcome::Transport(message),
            StatementResult::Error(message) => Outcome::Error(message),
        }
    }
}

pub trait Engine {
    /// Loads the fixture, replacing whatever the engine held.
    fn setup(&mut self, schema: &Schema, fixture: &Fixture) -> Result<()>;
    fn read(&mut self, query: &str) -> Result<Outcome>;
    /// Whether data modification can run in a transaction that is rolled back.
    fn transactional(&self) -> bool;
    /// Runs a data modification and then the digest queries in one transaction, and rolls it back.
    fn rolled_back(&mut self, mutation: &str, digests: &[String]) -> Result<Vec<Outcome>>;
    /// Chooses the graph with an undirected edge type, for engines that hold one.
    fn use_undirected(&mut self, _undirected: bool) {}
    fn restarts(&self) -> usize {
        0
    }
}

/// An engine's entry in config.yml.
#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct EngineConfig {
    /// duckgql, ladybug, neo4j_http, or opencypher_http.
    pub connector: String,
    /// A built-in profile (gql, cypher25, opencypher) or a profile file, relative to config.yml.
    pub dialect: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
    /// Features the engine has beyond its dialect profile.
    #[serde(default)]
    pub supported: Vec<String>,
    /// Features the engine lacks beyond its dialect profile.
    #[serde(default)]
    pub unsupported: Vec<String>,
    /// Syntax settings that override the profile's.
    #[serde(default)]
    pub syntax: Option<serde_yaml::Value>,
    /// Check the engine against itself with rewrites that must not change an answer.
    #[serde(default)]
    pub metamorphic: bool,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    /// Extra HTTP headers, for an endpoint behind an authenticating proxy.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Where an embedded engine keeps its database, relative to config.yml.
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// Loading the fixture deletes everything in a remote database; it must be allowed explicitly.
    #[serde(default)]
    pub allow_wipe: bool,
}

fn enabled() -> bool {
    true
}

pub fn connect(name: &str, config: &EngineConfig, base: &std::path::Path, timeout: Duration) -> Result<Box<dyn Engine>> {
    Ok(match config.connector.as_str() {
        "duckgql" => Box::new(DuckGql { worker: None, timeout, graph: "fuzz" }),
        "ladybug" => {
            let path = base.join(config.path.clone().unwrap_or_else(|| PathBuf::from(format!("runs/{name}.lbdb"))));
            Box::new(Ladybug { worker: None, timeout, path })
        }
        "neo4j_http" => {
            require_wipe(name, config)?;
            let url = config.url.clone().unwrap_or_else(|| "http://localhost:7474".to_string());
            let neo = Neo::new(&url, config.user.as_deref().unwrap_or("neo4j"), config.password.as_deref().unwrap_or(""), timeout * 3);
            Box::new(Neo4jHttp { neo })
        }
        "opencypher_http" => {
            require_wipe(name, config)?;
            let url = config.url.clone().with_context(|| format!("engine {name}: opencypher_http needs a url"))?;
            Box::new(OpenCypherHttp::new(url, config.headers.clone(), timeout * 3))
        }
        other => bail!("engine {name}: unknown connector '{other}' (duckgql, ladybug, neo4j_http, opencypher_http)"),
    })
}

fn require_wipe(name: &str, config: &EngineConfig) -> Result<()> {
    if !config.allow_wipe {
        bail!("engine {name}: loading the fixture deletes everything in its database; set allow_wipe: true to allow that");
    }
    Ok(())
}

fn worker_outcomes(outcome: RunOutcome, count: usize) -> Vec<Outcome> {
    match outcome {
        RunOutcome::Results(results) => results.into_iter().map(Outcome::from_statement).collect(),
        RunOutcome::Crash(detail) => vec![Outcome::Crash(detail); count],
        RunOutcome::Timeout => vec![Outcome::Timeout; count],
    }
}

// ------------------------------------------------------------------------- DuckGQL

/// DuckGQL in a worker, with two graphs: `fuzz`, which other engines also hold, and `fuzzu`, which adds an
/// undirected edge type.
struct DuckGql {
    worker: Option<Worker>,
    timeout: Duration,
    graph: &'static str,
}

impl Engine for DuckGql {
    fn setup(&mut self, _schema: &Schema, fixture: &Fixture) -> Result<()> {
        let mut statements = Vec::new();
        for (name, undirected) in [("fuzz", false), ("fuzzu", true)] {
            let schema = Schema::social(undirected);
            statements.push(schema.gql_create_graph(name));
            statements.push(format!("SESSION SET GRAPH {name}"));
            statements.extend(schema.fixture(fixture.seed).gql_statements());
        }
        let setup = statements.into_iter().map(|sql| Statement { sql, read: false }).collect();
        self.worker = Some(Worker::new(vec!["duckgql".to_string()], setup, self.timeout));
        Ok(())
    }

    fn read(&mut self, query: &str) -> Result<Outcome> {
        let statements = vec![Statement { sql: format!("SESSION SET GRAPH {}", self.graph), read: false }, Statement { sql: query.to_string(), read: true }];
        let outcomes = worker_outcomes(self.worker.as_mut().unwrap().run(&statements)?, 2);
        Ok(outcomes[1].clone())
    }

    fn transactional(&self) -> bool {
        true
    }

    fn rolled_back(&mut self, mutation: &str, digests: &[String]) -> Result<Vec<Outcome>> {
        let mut statements = vec![
            Statement { sql: format!("SESSION SET GRAPH {}", self.graph), read: false },
            Statement { sql: "BEGIN".to_string(), read: false },
            Statement { sql: mutation.to_string(), read: false },
        ];
        statements.extend(digests.iter().map(|digest| Statement { sql: digest.clone(), read: true }));
        statements.push(Statement { sql: "ROLLBACK".to_string(), read: false });
        let count = statements.len();
        let outcomes = worker_outcomes(self.worker.as_mut().unwrap().run(&statements)?, count);
        Ok(outcomes[2..count - 1].to_vec())
    }

    fn use_undirected(&mut self, undirected: bool) {
        self.graph = if undirected { "fuzzu" } else { "fuzz" };
    }

    fn restarts(&self) -> usize {
        self.worker.as_ref().map(|worker| worker.restarts).unwrap_or(0)
    }
}

// ------------------------------------------------------------------------- Ladybug

/// Ladybug through the lbug crate, in a worker. It needs node and relationship tables declared first.
struct Ladybug {
    worker: Option<Worker>,
    timeout: Duration,
    path: PathBuf,
}

fn ladybug_type(ty: Ty) -> &'static str {
    match ty {
        Ty::Int => "INT64",
        Ty::Float => "DOUBLE",
        Ty::Str => "STRING",
        Ty::Bool => "BOOLEAN",
    }
}

impl Engine for Ladybug {
    fn setup(&mut self, schema: &Schema, fixture: &Fixture) -> Result<()> {
        let mut statements = Vec::new();
        for node in &schema.nodes {
            let columns: Vec<String> = node.props.iter().map(|prop| format!("{} {}", prop.name, ladybug_type(prop.ty))).collect();
            statements.push(format!("CREATE NODE TABLE {}({}, PRIMARY KEY(id))", node.label, columns.join(", ")));
        }
        for edge in &schema.edges {
            let mut columns = vec![format!("FROM {} TO {}", edge.source, edge.target)];
            columns.extend(edge.props.iter().map(|prop| format!("{} {}", prop.name, ladybug_type(prop.ty))));
            statements.push(format!("CREATE REL TABLE {}({})", edge.label, columns.join(", ")));
        }
        statements.extend(fixture.cypher_statements().into_iter().skip(1));
        let setup = statements.into_iter().map(|sql| Statement { sql, read: false }).collect();
        let args = vec!["ladybug".to_string(), "--path".to_string(), self.path.display().to_string()];
        self.worker = Some(Worker::new(args, setup, self.timeout));
        Ok(())
    }

    fn read(&mut self, query: &str) -> Result<Outcome> {
        let statements = vec![Statement { sql: query.to_string(), read: true }];
        Ok(worker_outcomes(self.worker.as_mut().unwrap().run(&statements)?, 1).remove(0))
    }

    fn transactional(&self) -> bool {
        true
    }

    fn rolled_back(&mut self, mutation: &str, digests: &[String]) -> Result<Vec<Outcome>> {
        let mut statements = vec![Statement { sql: "BEGIN TRANSACTION".to_string(), read: false }, Statement { sql: mutation.to_string(), read: false }];
        statements.extend(digests.iter().map(|digest| Statement { sql: digest.clone(), read: true }));
        statements.push(Statement { sql: "ROLLBACK".to_string(), read: false });
        let count = statements.len();
        let outcomes = worker_outcomes(self.worker.as_mut().unwrap().run(&statements)?, count);
        Ok(outcomes[1..count - 1].to_vec())
    }

    fn restarts(&self) -> usize {
        self.worker.as_ref().map(|worker| worker.restarts).unwrap_or(0)
    }
}

// ------------------------------------------------------------------------- Neo4j

struct Neo4jHttp {
    neo: Neo,
}

impl Engine for Neo4jHttp {
    fn setup(&mut self, _schema: &Schema, fixture: &Fixture) -> Result<()> {
        for statement in fixture.cypher_statements() {
            if let StatementResult::Error(error) = self.neo.run(&statement)? {
                bail!("loading the Neo4j fixture failed: {error}\n{statement}");
            }
        }
        Ok(())
    }

    fn read(&mut self, query: &str) -> Result<Outcome> {
        Ok(Outcome::from_statement(self.neo.run(query)?))
    }

    fn transactional(&self) -> bool {
        true
    }

    fn rolled_back(&mut self, mutation: &str, digests: &[String]) -> Result<Vec<Outcome>> {
        let mut statements = vec![mutation.to_string()];
        statements.extend(digests.iter().cloned());
        Ok(self.neo.run_rolled_back(&statements)?.into_iter().map(Outcome::from_statement).collect())
    }
}

// ------------------------------------------------------------------------- openCypher over HTTP

/// An openCypher HTTP endpoint such as Amazon Neptune's `/openCypher`: one query per form-encoded POST,
/// answered with `{"results": [{column: value}, ...]}`. It has no transactions, so data modification is skipped.
struct OpenCypherHttp {
    agent: ureq::Agent,
    url: String,
    headers: BTreeMap<String, String>,
}

impl OpenCypherHttp {
    fn new(url: String, headers: BTreeMap<String, String>, timeout: Duration) -> OpenCypherHttp {
        let config = ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(timeout)).build();
        OpenCypherHttp { agent: config.into(), url, headers }
    }

    fn post(&self, query: &str) -> Outcome {
        let mut request = self.agent.post(&self.url);
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        let body: Value = match request.send_form([("query", query)]) {
            Ok(mut response) => match response.body_mut().with_config().limit(256 * 1024 * 1024).read_json() {
                Ok(body) => body,
                Err(error) => return Outcome::Transport(error.to_string()),
            },
            Err(error) => return Outcome::Transport(error.to_string()),
        };
        if let Some(results) = body.get("results").and_then(Value::as_array) {
            let columns: Vec<String> = results.first().and_then(Value::as_object).map(|row| row.keys().cloned().collect()).unwrap_or_default();
            let rows = results
                .iter()
                .map(|row| columns.iter().map(|column| row.get(column).map(Val::from_json).unwrap_or(Val::Null)).collect())
                .collect();
            return Outcome::Rows { columns, rows };
        }
        let code = body.get("code").and_then(Value::as_str).unwrap_or("error");
        let message = body.get("detailedMessage").or_else(|| body.get("message")).and_then(Value::as_str).unwrap_or("");
        Outcome::Error(format!("{code}: {message}"))
    }
}

impl Engine for OpenCypherHttp {
    fn setup(&mut self, _schema: &Schema, fixture: &Fixture) -> Result<()> {
        for statement in fixture.cypher_statements() {
            match self.post(&statement) {
                Outcome::Error(error) | Outcome::Transport(error) => bail!("loading the fixture failed: {error}\n{statement}"),
                _ => {}
            }
        }
        Ok(())
    }

    fn read(&mut self, query: &str) -> Result<Outcome> {
        Ok(self.post(query))
    }

    fn transactional(&self) -> bool {
        false
    }

    fn rolled_back(&mut self, _mutation: &str, _digests: &[String]) -> Result<Vec<Outcome>> {
        bail!("openCypher over HTTP has no transactions to roll back")
    }
}
