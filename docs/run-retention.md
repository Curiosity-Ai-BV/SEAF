# Run Retention And Audited Purge

SEAF bounds each run tree during publication and provides an explicit,
operator-authorized budget for stored runs. Purge is dry-run by default:

```sh
seaf loop purge --max-managed-bytes 1073741824 --json
```

Apply the exact policy only after reviewing the plan:

```sh
seaf loop purge --max-managed-bytes 1073741824 --apply --json
```

`0` is a valid budget. It requests deletion of every eligible run, but never
overrides a protection below.

## Storage Budgets

The existing per-run limits remain authoritative: 32 MiB total regular-file
bytes, 4,096 entries, eight descendant-directory levels, and semantic
per-artifact limits (2 MiB by default, with the smaller log and provider
artifact limits enforced by their owning writers).

`--max-managed-bytes` adds a runs-root budget. Managed bytes are the bounded
regular-file bytes in authenticated ordinary run directories, including
protected runs. Purge may finish above the requested budget when protected
bytes alone exceed it; the report makes the remaining byte count and every
protection explicit.

All dot-prefixed runs-root entries are outside the managed budget and are never
selected. Operator-excluded entries include migration intent, staged, and
backup siblings. The audit separately classifies
`.retention-purge.intent.json`, `.retention-purge.result.json`,
`.retention-purge.result.tmp`, and intent-owned
`.retention-purge.tombstone-v1-<digest>.deleting` tombstones as purge control
state. The fixed-length component is a domain-separated SHA-256 digest of the
intent-bound run and directory identity, so maximum-length authenticated run
IDs remain safe filesystem inputs. The intent loader re-derives the name before
recovery.

The runs root permits at most 4,096 operator-managed, protected, or excluded
entries. SEAF-owned retention controls have a separate explicit allowance of
four entries: intent, result, result temporary, and the one-run batch tombstone.
Traversal remains bounded at 4,100 names and fails closed on any excess or
unexpected control state. A migrated ordinary run that contains
`migration-v0-v1.result.json` is also protected, preserving the result and its
paired backup evidence. Purge does not manage unrelated non-dot files, non-UTF-8
names, symlinks, broad permissions, hard links, or malformed runs; it fails
closed on those entries. Every managed regular file must have exactly one link
before intent publication. A file hard-linked outside the run therefore makes
dry-run and apply fail without creating control state or unlinking either name.

## Retention Policy

Only authenticated runs with status `passed` or `completed` are eligible. An
otherwise eligible run remains protected when its isolated candidate workspace
is not `cleaned`, when it contains migration-result evidence, or when its
existing mutation lock is busy. `eval_passed` and `promoted` are protected final
authority: their candidates must remain active and the supported workflow does
not provide a cleanup transition. Every other run status is active for
retention purposes and is never selected.

Eligible runs are ordered by `updated_at` parsed as canonical decimal Unix
seconds and then by `run_id`. Unsupported, noncanonical, or overflowing
timestamps fail closed before intent publication. Purge selects the oldest runs
until managed bytes would be within the operator-supplied budget or no eligible
run remains. It never creates or cleans a run lock while observing eligibility,
and releases observation guards for unselected eligible runs before convergence
is measured.

Dry-run acquires only non-creating, non-blocking observations of existing
locks. It creates no intent, result, temporary, or tombstone and does not alter
run bytes. Its canonical JSON report records immutable decision evidence: the
managed inventory and bytes, protected runs, operator-excluded root entries,
normalized purge-control state, selected runs, byte projection, and summary
digest. Because dry-run does not converge a transaction, `converged` is null.

## Apply, Recovery, And Audit

Apply revalidates every selected run tree and typed run authority while holding
its exact existing mutation lock. It then publishes the bounded canonical
`.retention-purge.intent.json` before the first rename or deletion. A pending
intent accepts only an ordinary retry with the same byte budget; a conflicting
policy fails closed.

Each durable intent authorizes one selected directory. Valid multi-run plans
therefore execute as a sequence of recoverable batches until the requested
budget is reached or no eligible run remains. The next intent binds the prior
audit digest; each result carries the exact cumulative deleted-run summaries
and whether another batch is required. A verified continuation result remains
authenticated batch history even when unrelated root state arrives; current
snapshot equality is required only for exact final-result reuse. If a crash
occurs after a chained result rename but before intent unlink, retry adopts the
matching intent/result digest chain first, then makes a fresh decision from the
current root without duplicating or dropping a deleted ID. Final-batch adoption
uses the same rule: after intent unlink, fresh normalized inventory must still
equal the adopted convergence snapshot or the result becomes authenticated
prior history for a new same-policy decision. An eligible run arriving after
the rename is therefore purged before success is returned. A worst-case
4,096-entry run fits the 2 MiB intent cap because each manifest entry is
represented by a SHA-256 fingerprint over its path, kind, size, and content
digest. The cumulative audit has a separately proven 8 MiB cap covering 4,096
maximum-length run summaries and maximum protected-state snapshots.

Each selected directory is renamed through the pinned runs-root descriptor to
its deterministic private tombstone before recursive deletion. The intent
binds the directory identity and the complete compact manifest. After
interruption, retry accepts only the same identity and an exact fingerprint
subset, so substituted tombstones and newly injected files fail closed. If
provider records were already deleted, remaining `run.json` is still checked
as intrinsic typed authority and against its intent-bound manifest and digest;
retry does not require already-deleted relational artifacts. The typed
`run.json` and held mutation lock are removed last. A missing intent-owned
tombstone means that run already converged.

The intent binds the complete immutable decision snapshot, including every
protected run and operator-excluded root entry present when selection was made.
Those fields are never rewritten when the transaction resumes.

After every selected tombstone is gone, SEAF publishes the bounded canonical
`.retention-purge.result.json` by descriptor-relative atomic rename-overwrite,
syncs the runs root, and removes the intent. An interrupted replacement leaves
either the verified old final or the verified new final; it never unlinks the
old final first. The result
separately binds the original decision and intent, selected and deleted runs,
projected bytes, the final managed inventory and bytes, final protections and
operator exclusions, normalized final purge control state, and its own audit
digest. The normalized final control state is intent absent, result present,
temporary absent, and no tombstones. A run or excluded migration sibling
created while purge is interrupted therefore appears only in converged
evidence, preserving both why the earlier selection was made and what survived.

Exact retry returns the byte-identical verified result only after current
managed, protected, excluded, and purge-control state equals the recorded
convergence. Changed exclusions or controls start a fresh audited decision
instead of silently returning stale evidence. Tampered result bytes fail
closed.

The durable audit stores `.retention-purge.result.json` as a stable relative
identity. Verified runtime reports resolve it against the currently pinned runs
root, so moving the complete root does not return a stale absolute path. The
supported preview retains one latest cumulative batch result, capped at the
proven 8 MiB boundary. A later successful purge atomically replaces it only
after that batch converges. This is an operational latest-summary audit, not an
append-only compliance archive; operators who need historical retention must
copy verified result files into their own controlled archive.
