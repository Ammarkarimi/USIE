# 0004 — Content-addressed blob store behind an interface

## Decision
Source content, parsed IR and uploads are stored by key in a `blob.Store`.
Source keys are content hashes scoped by organization (`org/<org>/src/<sha256>`),
so identical files are stored once per tenant and never shared across
tenants. The only implementation is a filesystem directory with atomic
writes (temp file + rename) and a key grammar that cannot express traversal.

## Consequences
- The API and all workers must share the directory (a volume in the compose
  deployment). An S3-compatible implementation is planned and is the
  prerequisite for running workers on separate hosts without shared storage.
- The engine reads sources straight from the content-addressed directory,
  so files are never copied for parsing.
- There is no garbage collection of unreferenced blobs yet.
