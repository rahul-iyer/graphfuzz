//! Embedded engines run in a worker process, so a crash or a hang in an engine costs one query, not the
//! run. The parent sends JSON requests on the worker's stdin and reads JSON responses from its stdout;
//! `graphfuzz worker <backend>` is the worker.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::value::{Row, Val};

#[derive(Serialize, Deserialize, Clone)]
pub struct Statement {
    pub sql: String,
    /// Read its rows; otherwise run it for its effect.
    pub read: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum StatementResult {
    Rows { columns: Vec<String>, rows: Vec<Row> },
    Done,
    Error(String),
}

#[derive(Serialize, Deserialize)]
struct Request {
    statements: Vec<Statement>,
    timeout_ms: u64,
}

#[derive(Serialize, Deserialize)]
struct Response {
    results: Vec<StatementResult>,
}

#[derive(Debug)]
pub enum RunOutcome {
    Results(Vec<StatementResult>),
    Crash(String),
    Timeout,
}

/// Answers requests, running each statement with `run`.
fn serve(mut run: impl FnMut(&Statement, Duration) -> StatementResult) -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let request: Request = serde_json::from_str(&line?)?;
        let timeout = Duration::from_millis(request.timeout_ms);
        let results = request.statements.iter().map(|statement| run(statement, timeout)).collect();
        serde_json::to_writer(&mut stdout, &Response { results })?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
    }
    Ok(())
}

/// The worker's main loop for one backend.
pub fn worker_main(backend: &str, path: Option<PathBuf>) -> Result<()> {
    match backend {
        "duckgql" => {
            let connection = duckdb::Connection::open_in_memory()?;
            serve(|statement, timeout| {
                let interrupt = connection.interrupt_handle();
                let (done, finished) = channel::<()>();
                // Stops a statement that runs past its timeout; the parent kills the worker if this fails.
                let timer = thread::spawn(move || {
                    if let Err(RecvTimeoutError::Timeout) = finished.recv_timeout(timeout) {
                        interrupt.interrupt();
                    }
                });
                let result = run_duckdb(&connection, statement);
                let _ = done.send(());
                let _ = timer.join();
                result
            })
        }
        "ladybug" => {
            let path = path.context("the Ladybug worker needs a database path")?;
            // Every worker starts from an empty database, which the parent then loads. The write-ahead log and
            // other files beside it belong to the old database and must go too.
            if let (Some(parent), Some(name)) = (path.parent(), path.file_name().and_then(|name| name.to_str())) {
                std::fs::create_dir_all(parent)?;
                for entry in std::fs::read_dir(parent)?.flatten() {
                    if entry.file_name().to_str().is_some_and(|file| file == name || file.starts_with(&format!("{name}."))) {
                        let stale = entry.path();
                        std::fs::remove_dir_all(&stale).or_else(|_| std::fs::remove_file(&stale)).ok();
                    }
                }
            }
            let database = lbug::Database::new(&path, lbug::SystemConfig::default())?;
            let connection = lbug::Connection::new(&database)?;
            serve(|statement, timeout| {
                connection.set_query_timeout(timeout.as_millis() as u64);
                run_ladybug(&connection, statement)
            })
        }
        other => bail!("unknown worker backend '{other}'"),
    }
}

fn run_duckdb(connection: &duckdb::Connection, statement: &Statement) -> StatementResult {
    if !statement.read {
        return match connection.execute_batch(&statement.sql) {
            Ok(()) => StatementResult::Done,
            Err(error) => StatementResult::Error(error.to_string()),
        };
    }
    let mut prepared = match connection.prepare(&statement.sql) {
        Ok(prepared) => prepared,
        // A GQL statement that becomes several SQL statements runs as a batch, with no rows to read.
        Err(error) if error.to_string().contains("multiple statements") => {
            return match connection.execute_batch(&statement.sql) {
                Ok(()) => StatementResult::Done,
                Err(error) => StatementResult::Error(error.to_string()),
            };
        }
        Err(error) => return StatementResult::Error(error.to_string()),
    };
    let mut rows = match prepared.query([]) {
        Ok(rows) => rows,
        Err(error) => return StatementResult::Error(error.to_string()),
    };
    let columns: Vec<String> = rows.as_ref().map(|statement| statement.column_names()).unwrap_or_default();
    let mut result = Vec::new();
    loop {
        match rows.next() {
            Ok(Some(row)) => {
                let mut values = Vec::new();
                for index in 0..columns.len() {
                    match row.get::<_, duckdb::types::Value>(index) {
                        Ok(value) => values.push(Val::from_duckdb(value)),
                        Err(error) => return StatementResult::Error(format!("reading column {index}: {error}")),
                    }
                }
                result.push(values);
            }
            Ok(None) => break,
            Err(error) => return StatementResult::Error(error.to_string()),
        }
    }
    StatementResult::Rows { columns, rows: result }
}

fn run_ladybug(connection: &lbug::Connection, statement: &Statement) -> StatementResult {
    match connection.query(&statement.sql) {
        Ok(result) => {
            if !statement.read {
                return StatementResult::Done;
            }
            let columns = result.get_column_names();
            let rows = result.map(|row| row.into_iter().map(ladybug_value).collect()).collect();
            StatementResult::Rows { columns, rows }
        }
        Err(error) => StatementResult::Error(error.to_string()),
    }
}

fn ladybug_value(value: lbug::Value) -> Val {
    use lbug::Value as L;
    match value {
        L::Null(_) => Val::Null,
        L::Bool(value) => Val::Bool(value),
        L::Int64(value) => Val::Num(value as f64),
        L::Int32(value) => Val::Num(value as f64),
        L::Int16(value) => Val::Num(value as f64),
        L::Int8(value) => Val::Num(value as f64),
        L::UInt64(value) => Val::Num(value as f64),
        L::UInt32(value) => Val::Num(value as f64),
        L::UInt16(value) => Val::Num(value as f64),
        L::UInt8(value) => Val::Num(value as f64),
        L::Int128(value) => Val::Num(value as f64),
        L::Double(value) => Val::Num(value),
        L::Float(value) => Val::Num(value as f64),
        L::Decimal(value) => Val::Num(value.to_string().parse().unwrap_or(f64::NAN)),
        L::String(text) => Val::Str(text),
        L::List(_, items) | L::Array(_, items) => Val::List(items.into_iter().map(ladybug_value).collect()),
        L::Struct(fields) => Val::Map(fields.into_iter().map(|(name, value)| (name, ladybug_value(value))).collect()),
        L::Node(node) => Val::node(
            vec![node.get_label_name().clone()],
            node.get_properties().iter().map(|(name, value)| (name.clone(), ladybug_value(value.clone()))).collect(),
        ),
        L::Rel(rel) => Val::relationship(
            rel.get_label_name(),
            rel.get_properties().iter().map(|(name, value)| (name.clone(), ladybug_value(value.clone()))).collect(),
        ),
        L::RecursiveRel { rels, .. } => Val::path(
            rels.iter()
                .map(|rel| Val::relationship(rel.get_label_name(), rel.get_properties().iter().map(|(name, value)| (name.clone(), ladybug_value(value.clone()))).collect()))
                .collect(),
        ),
        other => Val::Other(format!("{other}")),
    }
}

/// The parent's handle on a worker, restarted with the setup statements after a crash or a hang.
pub struct Worker {
    args: Vec<String>,
    setup: Vec<Statement>,
    timeout: Duration,
    child: Option<(Child, ChildStdin, Receiver<String>)>,
    pub restarts: usize,
}

impl Worker {
    pub fn new(args: Vec<String>, setup: Vec<Statement>, timeout: Duration) -> Worker {
        Worker { args, setup, timeout, child: None, restarts: 0 }
    }

    fn start(&mut self) -> Result<()> {
        let exe = std::env::current_exe()?;
        let mut child = Command::new(exe)
            .arg("worker")
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // An engine that fails to start says why on stderr.
            .stderr(Stdio::inherit())
            .spawn()
            .context("starting an engine worker")?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (lines, receiver) = channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if lines.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        self.child = Some((child, stdin, receiver));
        let setup = self.setup.clone();
        match self.send(&setup, Duration::from_secs(300))? {
            RunOutcome::Results(results) => {
                for (statement, result) in setup.iter().zip(&results) {
                    if let StatementResult::Error(error) = result {
                        bail!("{} fixture setup failed: {error}\n{}", self.args.join(" "), statement.sql);
                    }
                }
                Ok(())
            }
            other => bail!("{} fixture setup did not finish: {other:?}", self.args.join(" ")),
        }
    }

    fn stop(&mut self) {
        if let Some((mut child, _, _)) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn send(&mut self, statements: &[Statement], wait: Duration) -> Result<RunOutcome> {
        let (child, stdin, receiver) = self.child.as_mut().unwrap();
        let request = Request { statements: statements.to_vec(), timeout_ms: self.timeout.as_millis() as u64 };
        let mut line = serde_json::to_string(&request)?;
        line.push('\n');
        if stdin.write_all(line.as_bytes()).and_then(|_| stdin.flush()).is_err() {
            let status = child.try_wait().ok().flatten();
            self.stop();
            return Ok(RunOutcome::Crash(format!("worker exited: {status:?}")));
        }
        match receiver.recv_timeout(wait) {
            Ok(line) => {
                let response: Response = serde_json::from_str(&line)?;
                Ok(RunOutcome::Results(response.results))
            }
            Err(RecvTimeoutError::Timeout) => {
                self.stop();
                Ok(RunOutcome::Timeout)
            }
            Err(RecvTimeoutError::Disconnected) => {
                let status = child.wait().ok();
                self.child = None;
                Ok(RunOutcome::Crash(format!("worker exited: {status:?}")))
            }
        }
    }

    /// Runs statements in order; a crashed or hung worker is replaced before the next run.
    pub fn run(&mut self, statements: &[Statement]) -> Result<RunOutcome> {
        if self.child.is_none() {
            self.start()?;
        }
        // The worker interrupts a statement at the timeout; waiting twice as long per statement allows for that.
        let wait = self.timeout * (2 * statements.len() as u32 + 1);
        let outcome = self.send(statements, wait)?;
        if !matches!(outcome, RunOutcome::Results(_)) {
            self.restarts += 1;
        }
        Ok(outcome)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}
