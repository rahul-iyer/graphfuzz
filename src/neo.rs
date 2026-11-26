//! Neo4j through its HTTP Query API (v2). Data-modifying cases run in an explicit transaction that is
//! rolled back, so every case sees the same fixture.

use std::time::Duration;

use anyhow::Result;
use base64::Engine;
use serde_json::{Value, json};

use crate::value::Val;
use crate::worker::StatementResult;

/// The error code of a request that did not reach Neo4j or whose answer could not be read.
pub const TRANSPORT_ERROR: &str = "graphfuzz.Transport";

pub struct Neo {
    agent: ureq::Agent,
    base: String,
    authorization: String,
}

impl Neo {
    pub fn new(url: &str, user: &str, password: &str, timeout: Duration) -> Neo {
        let config = ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(timeout)).build();
        Neo {
            agent: config.into(),
            base: format!("{}/db/neo4j/query/v2", url.trim_end_matches('/')),
            authorization: format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))),
        }
    }

    fn post(&self, url: &str, statement: &str) -> Result<Value> {
        let response = self.agent.post(url).header("Authorization", &self.authorization).send_json(json!({ "statement": statement }));
        // A failed request is reported as an error result, so one oversized answer does not stop a run.
        let transport = |error: String| json!({ "errors": [{ "code": TRANSPORT_ERROR, "message": error }] });
        match response {
            Ok(mut response) => match response.body_mut().with_config().limit(256 * 1024 * 1024).read_json::<Value>() {
                Ok(body) => Ok(body),
                Err(error) => Ok(transport(error.to_string())),
            },
            Err(error) => Ok(transport(error.to_string())),
        }
    }

    fn result(body: &Value) -> StatementResult {
        if let Some(errors) = body.get("errors").and_then(Value::as_array)
            && let Some(error) = errors.first()
        {
            let code = error.get("code").and_then(Value::as_str).unwrap_or("");
            let message = error.get("message").and_then(Value::as_str).unwrap_or("");
            return StatementResult::Error(format!("{code}: {message}"));
        }
        let data = &body["data"];
        let columns = data["fields"].as_array().map(|fields| fields.iter().filter_map(|field| field.as_str().map(String::from)).collect()).unwrap_or_default();
        let rows = data["values"]
            .as_array()
            .map(|rows| rows.iter().map(|row| row.as_array().map(|values| values.iter().map(Val::from_json).collect()).unwrap_or_default()).collect())
            .unwrap_or_default();
        StatementResult::Rows { columns, rows }
    }

    /// Runs one statement in its own transaction.
    pub fn run(&self, statement: &str) -> Result<StatementResult> {
        Ok(Self::result(&self.post(&self.base, statement)?))
    }

    /// Runs statements in one transaction and rolls it back.
    pub fn run_rolled_back(&self, statements: &[String]) -> Result<Vec<StatementResult>> {
        let mut results = Vec::new();
        let mut transaction: Option<String> = None;
        for statement in statements {
            let url = match &transaction {
                Some(id) => format!("{}/tx/{}", self.base, id),
                None => format!("{}/tx", self.base),
            };
            let body = self.post(&url, statement)?;
            if transaction.is_none() {
                transaction = body["transaction"]["id"].as_str().map(String::from);
            }
            let result = Self::result(&body);
            let failed = matches!(result, StatementResult::Error(_));
            results.push(result);
            // A failed statement ends the transaction on the server.
            if failed {
                transaction = None;
                break;
            }
        }
        if let Some(id) = transaction {
            let _ = self.agent.delete(format!("{}/tx/{}", self.base, id)).header("Authorization", &self.authorization).call();
        }
        Ok(results)
    }
}
