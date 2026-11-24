//! DuckGQL runs in a worker process, so a crash or a hang in the engine costs one query, not the run.
//! The parent sends JSON requests on the worker's stdin and reads JSON responses from its stdout.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
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

/// The worker's loop: each request runs on one in-memory database, statement by statement.
pub fn worker_main() -> Result<()> {
    let connection = duckdb::Connection::open_in_memory()?;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let request: Request = serde_json::from_str(&line?)?;
        let mut results = Vec::new();
        for statement in &request.statements {
            let interrupt = connection.interrupt_handle();
            let (done, finished) = channel::<()>();
            let timeout = Duration::from_millis(request.timeout_ms);
            // Stops a statement that runs past its timeout; the parent kills the worker if this fails.
            let timer = thread::spawn(move || {
                if let Err(RecvTimeoutError::Timeout) = finished.recv_timeout(timeout) {
                    interrupt.interrupt();
                }
            });
            results.push(run_statement(&connection, statement));
            let _ = done.send(());
            let _ = timer.join();
        }
        serde_json::to_writer(&mut stdout, &Response { results })?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
    }
    Ok(())
}

fn run_statement(connection: &duckdb::Connection, statement: &Statement) -> StatementResult {
    if !statement.read {
        return match connection.execute_batch(&statement.sql) {
            Ok(()) => StatementResult::Done,
            Err(error) => StatementResult::Error(error.to_string()),
        };
    }
    let mut prepared = match connection.prepare(&statement.sql) {
        Ok(prepared) => prepared,
        Err(error) => return StatementResult::Error(error.to_string()),
    };
    let mut rows = match prepared.query([]) {
        Ok(rows) => rows,
        Err(error) => return StatementResult::Error(error.to_string()),
    };
    let mut result = Vec::new();
    let columns: Vec<String> = rows.as_ref().map(|statement| statement.column_names()).unwrap_or_default();
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

/// The parent's handle on a worker, restarted with the setup statements after a crash or a hang.
pub struct Duck {
    setup: Vec<Statement>,
    timeout: Duration,
    child: Option<(Child, ChildStdin, Receiver<String>)>,
    pub restarts: usize,
}

impl Duck {
    pub fn new(setup: Vec<Statement>, timeout: Duration) -> Duck {
        Duck { setup, timeout, child: None, restarts: 0 }
    }

    fn start(&mut self) -> Result<()> {
        let exe = std::env::current_exe()?;
        let mut child = Command::new(exe)
            .arg("worker")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("starting the DuckGQL worker")?;
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
        match self.send(&setup, Duration::from_secs(120))? {
            RunOutcome::Results(results) => {
                if let Some(StatementResult::Error(error)) = results.iter().find(|result| matches!(result, StatementResult::Error(_))) {
                    anyhow::bail!("DuckGQL fixture setup failed: {error}");
                }
                Ok(())
            }
            other => anyhow::bail!("DuckGQL fixture setup did not finish: {other:?}"),
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

impl Drop for Duck {
    fn drop(&mut self) {
        self.stop();
    }
}
