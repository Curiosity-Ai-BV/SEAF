# Durable Artifact Migration

`seaf loop migrate --runs-root <root> --run-id <id> [--json]` upgrades one
authenticated legacy run from the implicit v0 durable contract to explicit
schema version 1. The command migrates the run as one unit; there is no
supported per-file migration.

The managed contract occurrences are the authoritative ticket input and
snapshot, policy input, LoopRun, policy decisions embedded in authenticated
typed Development evidence or stored under managed patch-attempt names, and
EvalReports selected by the run. Reference discovery is seeded from the typed
LoopRun and continues only through exact typed owners such as EvalReport,
TestingEvidence, ApprovedEvaluationIntent, and ProviderExchangeRecord. Generic
reachable JSON is authenticated and canonicalized but does not contribute
nested authority merely because it contains fields named `path`, `digest`, or
`*_digest`. Digest projection runs only inside validated typed holders and
changes their declared references, historical bindings, and the LoopRun
input-digest map. Matching strings in payload fields and arbitrary objects
merely named `policy_decision` remain unchanged. Malformed managed Development
evidence fails closed. Unreferenced JSON/YAML and unrelated forensic or recovery
files are copied byte-for-byte and are not interpreted as migration inputs.

Before staging, the command validates the selected run ID, typed schemas,
canonical input bytes, fixed input digests, managed decision/report identity,
every reachable path/digest reference, and the existing artifact-storage
limits: each file's semantic byte cap, 32 MiB aggregate regular-file bytes,
4,096 total entries, and eight descendant directory levels. File metadata is
checked before a bounded read; tree inventories retain only length and a
streamed SHA-256 digest rather than file bodies. Directory enumeration stops
at entry 4,097 before retaining or sorting another name, so adversarially wide
directories cannot force unbounded name storage. An over-limit source fails
before intent or staged-directory creation. The same bounds are enforced while
copying and while validating staged, recovered, and completed trees. Missing
`schema_version` is legacy v0. Explicit version 0, future versions, malformed
versions, unknown current fields, unsafe paths, conflicting references, and
digest mismatches fail before publication without changing the selected run.
Current v1 runs are a byte-inert no-op.

Before publishing the intent, migration deterministically computes every
canonical rewrite byte, the permanent result bytes, and the transient ownership
marker bytes. Their exact file-size deltas and two new root entries are applied
to the pinned source inventory and checked against the semantic per-file,
aggregate byte, entry, and existing depth limits. The staged inventory must
equal this projection before the first rename. Its canonical digest is stored
in the closed durable intent, so retry compares recovery candidates to the
original admitted inventory rather than to a mutable result list. A legacy source that already
contains the reserved result or ownership path fails byte-inert before intent
creation.

Every managed or authenticated path is opened component-by-component beneath a
pinned private run directory. A symlink in any traversed ancestor fails before
intent creation; managed artifact discovery is also no-follow. Symlinks that
are unrelated to all authenticated paths are preserved as symlinks by the full
tree copy and are never traversed.

Migration acquires the run's existing permanent mutation lock before reading
authority and holds it through publication. It does not create a missing lock
or reclaim replacement temporaries during preflight, because either operation
would change the tree later promised as the byte-exact backup. A missing or
busy lock therefore fails before intent creation with repair context. The
copied staged run's lock is acquired before any staged rewrite and held through
its rename into place. Both locks remain bound to the exact child directory of
a pinned runs-root descriptor. Their child identity is revalidated before and
after authentication, copy, rewrite, and publication boundaries; replacing a
locked selected or staged path cannot redirect migration into an unlocked
directory.

## Transaction And Recovery

For run `<id>`, the command owns these deterministic siblings under the runs
root:

- `.<id>.migration-v0-v1.intent.json`
- `.<id>.migration-v0-v1.staged`
- `.<id>.migration-v0-v1.backup`

The canonical, closed typed intent binds the run ID, migration ID, target
version, source `run.json` digest, and a deterministic no-follow source-tree
digest. The tree digest covers sorted relative paths, entry types, streamed
regular-file digests and lengths, and symlink targets. Intent creation, reading,
and unlinking use the retained pinned runs-root descriptor and reject a rebound
root rather than operating on its replacement. The staged directory is a full
no-follow copy through pinned source and target descriptors. It carries a
canonical ownership marker bound to this intent's migration ID, run ID, and a
fresh token. Recovery authenticates that marker through the retained staged
descriptor before adopting or deleting the tree; a substituted or rebound
staged path without the exact proof is preserved for operator inspection. The
marker is transient, excluded from the changed-artifact audit, and removed
through the pinned staged descriptor before successful completion. After its
authenticated graph and any final-evaluation authority pass existing validators,
publication uses the pinned runs-root descriptor to rename the identity-checked
selected child to the backup and then the identity-checked staged child to the
selected name. The successful run contains a canonical, closed
typed `migration-v0-v1.result.json` carrying the same source bindings plus the
migrated `run.json` digest. Its sorted artifact list must equal the actual set
of changed regular files between retained backup and migrated source, excluding
only the result itself. The comparison reuses the same pinned no-follow tree
inventory as the audit digest and rejects missing/extra paths, type changes,
symlink-target changes, and false changed entries. The backup is the byte-exact
pre-migration tree and is retained for operator audit; successful retry does
not replace or delete it.

Recovery is derived from sibling topology:

- source plus intent only: remove the unused intent and restart;
- source plus staged plus intent: adopt a valid staged tree; an invalid or
  incomplete unpublished staged tree is removed through its pinned descriptor
  and rebuilt in the same ordinary retry; a self-consistent candidate whose
  inventory differs from the intent-bound projection is preserved and refused;
- backup plus staged plus intent: finish the staged-to-source rename;
- source plus backup plus intent: validate the published source and remove the
  completed intent; the ownership marker may still be present or may already
  have been durably removed, but both states require full result, migrated
  authority, backup/source binding, exact changed-artifact, and intent
  validation; when absent, only the exact intent-derived transient marker entry
  is reconstructed before comparing the completed source to the bound projected
  inventory digest; both selected and backup guards are then revalidated as
  exact children of the pinned runs root immediately before marker or intent
  cleanup; when the marker is present, both guards are revalidated again after
  its durable removal and immediately before intent unlink;
- source plus backup without intent: report current only when the source is a
  valid completed migration, its result is canonical and closed, its migrated
  run digest matches, and its source run/tree bindings match the retained
  backup and exact changed-artifact inventory;
- any other combination: fail as ambiguous and preserve every path for
  operator inspection.

A normal pre-publication error removes only this transaction's unused intent
and a staged directory whose retained identity and ownership marker still prove
that this invocation created it. Unproven staged replacements are never
deleted. It never removes the selected source. Once the first
rename has happened, retry completes or validates publication; it never deletes
the backup.

Deterministic interruption coverage includes the first recursive-copy entry and
the first in-place staged rewrite, in addition to intent publication, validated
staging, the interval between both renames, and selected-run publication. The
mid-copy and mid-rewrite retries prove convergence, byte-exact backup retention,
valid result/audit authority, and cleanup of transient siblings.

Every recovery topology validates the intent against the directory containing
the original authority: selected source before the first rename, retained
backup after it. A substituted source, backup, staged result, or intent fails
without another rename or adoption.

Legacy terminal runs that already contain evaluation-recovery history are
refused before the migration intent is created. Their exact historical Approved
authority is reconstructed from recovery source artifacts rather than only the
selected run, and v0-to-v1 projection for that lineage is not yet supported.
The refusal preserves all bytes and tells the operator to use the SEAF version
that created the recovery authority. Unrecovered EvalPassed history is
supported and is revalidated with the existing final-authority loader before
publication. A realistic recovered EvalPassed fixture exercises the unsupported
boundary and proves refusal is pre-transaction and byte-inert.
