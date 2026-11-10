# Submission Readiness

## Recommendation

Status: **ready for one controlled attempt**, with the explicit caveat that the
hidden cases, official performance baseline, and Agent-review aggregation are
unavailable. This is not a claim of guaranteed 80/80.

## Functional gates

- Official JSONL startup, execute, error, and shutdown protocol implemented.
- All 20 public cases and 95 public SQL statements pass.
- All 70 local functional tests pass; one benchmark is explicitly ignored by
  the normal test command and run separately.
- Linux Docker image build, complete Rust test suite, official handshake, and
  all public Docker cases pass in CI.
- BIGINT and request-ID JSON types retain precision.
- SQL errors return one semantic response and do not terminate the process.
- Shutdown rolls back an open transaction.
- Forced-kill recovery preserves committed rows and drops uncommitted rows.
- Invalid dimensions, NaN, and float32 overflow are rejected without partial
  writes.
- Delete/reinsert and failed-vector-insert index/storage regressions are fixed.
- Benchmark workload uses 10,000 rows, 128 dimensions, 1,000 queries, and
  Recall@10 = 1.0.

## Evidence state

- Agent implementation and analysis are explicitly disclosed in
  `conversation.json`.
- One student-executed reproduction run is recorded at commit
  `dccdbc95893da685f6faac41b7d7e2b1c922391d`.
- The student completed the transaction and duplicate-index follow-up questions
  in the student's own words.
- The evidence does not claim that Agent work proves independent student
  implementation or hidden-case verification.

## Remaining uncertainty

- Nine hidden functional cases are unavailable.
- The official performance baseline and cgroup measurements are unavailable.
- Local Windows timings cannot be converted to an official ranking score.
- `ann` currently returns exact TopK, protecting recall but limiting large-data
  query performance.
- Scalar rows remain in memory; vector payloads are page-backed.
- The Agent prompt score is expected to be bounded by the amount of genuine
  student-supported evidence.

## Submission checklist

- [x] Branch is `2026-competition-round2`.
- [x] Source builds from the Dockerfile.
- [x] Runtime uses stdin/stdout JSONL and writes durable data below `/data`.
- [x] Latest pushed HEAD matches the local HEAD.
- [x] Latest Linux Docker CI is green.
- [x] `conversation.json` is valid, below 128 KiB, and explicitly attributes
  Agent work.
- [x] One real student reproduction run and clarifications are captured.
- [ ] Student explicitly authorizes use of the single official submission.
- [ ] Submission form is filled with the final pushed 40-character SHA.

The remaining checkboxes are authorization and platform actions, not software
development tasks.
