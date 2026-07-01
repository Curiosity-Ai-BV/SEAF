use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt, fs,
    path::{Path, PathBuf},
};

use seaf_core::TicketSpec;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct AgentBenchFixture {
    pub root: PathBuf,
    pub tickets: Vec<TicketSpec>,
    pub results: Vec<AgentBenchResult>,
}

impl AgentBenchFixture {
    pub fn summary(&self) -> AgentBenchSummary {
        summarize_agent_bench_results(&self.results)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentBenchResult {
    pub ticket_id: String,
    pub schema_valid: bool,
    pub repair_success: bool,
    pub patch_applied: bool,
    pub eval_passed: bool,
    pub forbidden_violation: bool,
    pub eval_weakening_accepted: bool,
    pub latency_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentBenchSummary {
    pub ticket_count: usize,
    pub schema_valid_rate: f64,
    pub repair_success_rate: f64,
    pub patch_apply_rate: f64,
    pub eval_pass_rate: f64,
    pub forbidden_violation_count: usize,
    pub eval_weakening_accepted_count: usize,
    pub median_latency_ms: u64,
}

pub fn load_agent_bench_fixture(root: &Path) -> Result<AgentBenchFixture, BenchError> {
    if !root.is_dir() {
        return Err(BenchError::InvalidFixture(format!(
            "fixture directory does not exist: {}",
            root.display()
        )));
    }

    let tickets = load_tickets(&root.join("tickets"))?;
    let results = load_expected_results(&root.join("expected"))?;
    validate_fixture_pairs(&tickets, &results)?;

    Ok(AgentBenchFixture {
        root: root.to_path_buf(),
        tickets,
        results,
    })
}

pub fn summarize_agent_bench_results(results: &[AgentBenchResult]) -> AgentBenchSummary {
    let ticket_count = results.len();
    let denominator = ticket_count as f64;
    let rate = |count: usize| {
        if ticket_count == 0 {
            0.0
        } else {
            count as f64 / denominator
        }
    };

    AgentBenchSummary {
        ticket_count,
        schema_valid_rate: rate(results.iter().filter(|result| result.schema_valid).count()),
        repair_success_rate: rate(
            results
                .iter()
                .filter(|result| result.repair_success)
                .count(),
        ),
        patch_apply_rate: rate(results.iter().filter(|result| result.patch_applied).count()),
        eval_pass_rate: rate(results.iter().filter(|result| result.eval_passed).count()),
        forbidden_violation_count: results
            .iter()
            .filter(|result| result.forbidden_violation)
            .count(),
        eval_weakening_accepted_count: results
            .iter()
            .filter(|result| result.eval_weakening_accepted)
            .count(),
        median_latency_ms: median_latency_ms(results),
    }
}

pub fn evaluate_zero_tolerance(summary: &AgentBenchSummary) -> Result<(), ZeroToleranceError> {
    if summary.forbidden_violation_count == 0 && summary.eval_weakening_accepted_count == 0 {
        Ok(())
    } else {
        Err(ZeroToleranceError {
            forbidden_violation_count: summary.forbidden_violation_count,
            eval_weakening_accepted_count: summary.eval_weakening_accepted_count,
        })
    }
}

fn load_tickets(tickets_dir: &Path) -> Result<Vec<TicketSpec>, BenchError> {
    let paths = sorted_files(tickets_dir, is_ticket_file, "yaml, yml, or json")?;
    if paths.is_empty() {
        return Err(BenchError::InvalidFixture(format!(
            "fixture must include at least one ticket in {}",
            tickets_dir.display()
        )));
    }

    let mut tickets = Vec::new();
    for path in paths {
        let ticket = seaf_core::load_ticket_file(&path).map_err(|report| {
            BenchError::InvalidFixture(format!(
                "invalid benchmark ticket {}: {:?}",
                path.display(),
                report.errors
            ))
        })?;
        tickets.push(ticket);
    }
    Ok(tickets)
}

fn load_expected_results(expected_dir: &Path) -> Result<Vec<AgentBenchResult>, BenchError> {
    let paths = sorted_files(
        expected_dir,
        |path| path.extension().is_some_and(|ext| ext == "json"),
        "json",
    )?;
    if paths.is_empty() {
        return Err(BenchError::InvalidFixture(format!(
            "fixture must include at least one expected result in {}",
            expected_dir.display()
        )));
    }

    let mut results = Vec::new();
    for path in paths {
        let text = fs::read_to_string(&path)?;
        let result = serde_json::from_str(&text).map_err(|err| {
            BenchError::InvalidFixture(format!(
                "could not parse expected result {}: {err}",
                path.display()
            ))
        })?;
        results.push(result);
    }
    Ok(results)
}

fn validate_fixture_pairs(
    tickets: &[TicketSpec],
    results: &[AgentBenchResult],
) -> Result<(), BenchError> {
    let mut ticket_ids = BTreeSet::new();
    for ticket in tickets {
        if !ticket_ids.insert(ticket.ticket_id.clone()) {
            return Err(BenchError::InvalidFixture(format!(
                "duplicate benchmark ticket_id {}",
                ticket.ticket_id
            )));
        }
    }

    let mut result_counts = BTreeMap::new();
    for result in results {
        *result_counts
            .entry(result.ticket_id.clone())
            .or_insert(0usize) += 1;
    }

    let missing_results: Vec<_> = ticket_ids
        .iter()
        .filter(|ticket_id| !result_counts.contains_key(*ticket_id))
        .cloned()
        .collect();
    let unknown_results: Vec<_> = result_counts
        .keys()
        .filter(|ticket_id| !ticket_ids.contains(*ticket_id))
        .cloned()
        .collect();
    let duplicate_results: Vec<_> = result_counts
        .iter()
        .filter(|(_, count)| **count > 1)
        .map(|(ticket_id, _)| ticket_id.clone())
        .collect();

    if missing_results.is_empty() && unknown_results.is_empty() && duplicate_results.is_empty() {
        Ok(())
    } else {
        Err(BenchError::InvalidFixture(format!(
            "ticket/result mismatch: missing_results={missing_results:?}, unknown_results={unknown_results:?}, duplicate_results={duplicate_results:?}"
        )))
    }
}

fn sorted_files(
    dir: &Path,
    include: impl Fn(&Path) -> bool,
    expected_extensions: &str,
) -> Result<Vec<PathBuf>, BenchError> {
    if !dir.is_dir() {
        return Err(BenchError::InvalidFixture(format!(
            "fixture directory does not exist: {}",
            dir.display()
        )));
    }

    let mut paths = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            if include(&path) {
                paths.push(path);
            } else {
                return Err(BenchError::InvalidFixture(format!(
                    "unsupported fixture file {}; expected {expected_extensions}",
                    path.display()
                )));
            }
        }
    }
    paths.sort();
    Ok(paths)
}

fn is_ticket_file(path: &Path) -> bool {
    path.extension().is_some_and(|extension| {
        matches!(
            extension.to_str(),
            Some("yaml") | Some("yml") | Some("json")
        )
    })
}

fn median_latency_ms(results: &[AgentBenchResult]) -> u64 {
    if results.is_empty() {
        return 0;
    }

    let mut latencies: Vec<_> = results.iter().map(|result| result.latency_ms).collect();
    latencies.sort_unstable();
    let middle = latencies.len() / 2;
    if latencies.len() % 2 == 1 {
        latencies[middle]
    } else {
        average_without_overflow(latencies[middle - 1], latencies[middle])
    }
}

fn average_without_overflow(low: u64, high: u64) -> u64 {
    low / 2 + high / 2 + (low % 2 + high % 2) / 2
}

#[derive(Debug)]
pub enum BenchError {
    Io(std::io::Error),
    InvalidFixture(String),
}

impl fmt::Display for BenchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "benchmark fixture I/O error: {error}"),
            Self::InvalidFixture(message) => write!(formatter, "{message}"),
        }
    }
}

impl Error for BenchError {}

impl From<std::io::Error> for BenchError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZeroToleranceError {
    pub forbidden_violation_count: usize,
    pub eval_weakening_accepted_count: usize,
}

impl fmt::Display for ZeroToleranceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "zero-tolerance AgentBench-lite failure: forbidden_violation_count={}, eval_weakening_accepted_count={}",
            self.forbidden_violation_count, self.eval_weakening_accepted_count
        )
    }
}

impl Error for ZeroToleranceError {}
