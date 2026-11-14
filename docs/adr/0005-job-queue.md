# 0005 — PostgreSQL job queue instead of Kafka or Temporal

## Context
Analyses are long-running, must survive worker crashes, be cancellable,
retry transient failures and not retry permanent ones.

## Decision
A table (`analysis_jobs`) with `FOR UPDATE SKIP LOCKED` claiming, leases
extended by heartbeats, lease-expiry recovery, exponential backoff for
retryable failures, a `Permanent` error class, and cooperative cancellation.
Delivery is at-least-once; the pipeline is idempotent.

## Why not Kafka or Temporal now
One job kind with a linear pipeline does not need a workflow engine, and the
queue's state must be transactional with versions and results, which a table
gives for free. Temporal becomes attractive with multi-step workflows that
span services (transformation → build → test → review); Kafka with
high-volume event fan-out. Both are deferred, not rejected.

## Consequences
Throughput is bounded by PostgreSQL, which is ample for analysis jobs that
take seconds to minutes.
