# Issue Tracker (synthetic fixture)

This file exists to exercise the markdown extractors in `src/metrics/`.
The data is entirely fictional — no real cases, PRs, or people are
referenced. Schema mirrors the shape of a long-running issue-tracker
loop's `tracker.md`: heading → table.

Last updated: 2099-01-01 00:00 UTC

## To Ship

### Active Change Cases (PR still OPEN)

Cases whose PR is still open — work not yet shipped.

| Change Case | PR | Status | Notes |
|---|---|---|---|
| ISSUE-101 | example/repo#1001 | Pending Peer Review | Synthetic row — used for table-row counting. |
| ISSUE-102 | example/repo#1002 | Pending Manager Review | Synthetic row. |
| ISSUE-103 | example/repo#1003 | New/Unassigned | Synthetic row. |
| ISSUE-104 | example/repo#1004 | Pending Peer Review | Synthetic row. |

### PRs Missing a Change Case

Open PRs without an associated case. Synthetic rows.

| PR | Title | Created | Change Case |
|---|---|---|---|
| example/repo#2001 | Add example feature A | 2099-01-02 | MISSING |
| example/repo#2002 | Refactor example module B | 2099-01-02 | MISSING |
| example/repo#2003 | Fix example bug C | 2099-01-03 | MISSING |
| example/repo#2004 | Update example dependency D | 2099-01-03 | MISSING |
| example/repo#2005 | Add example test E | 2099-01-04 | MISSING |
| example/repo#2006 | Improve example docs F | 2099-01-04 | MISSING |
| example/repo#2007 | Migrate example config G | 2099-01-05 | MISSING |
| example/repo#2008 | Replace example dep H | 2099-01-05 | MISSING |
| example/repo#2009 | Tune example perf I | 2099-01-06 | MISSING |
| example/repo#2010 | Reorganize example dir J | 2099-01-06 | MISSING |
| example/repo#2011 | Drop example flag K | 2099-01-07 | MISSING |
| example/repo#2012 | Tidy example tests L | 2099-01-07 | MISSING |

### Excluded from MISSING list (out of scope)

- example/docs-only-repo PRs — docs-only repo; cases not required.
- Draft / WIP / DO-NOT-MERGE PRs.

---

## Shipped

### Change Cases Pending Close (PR already merged/closed)

Cases whose PR shipped — just need a status update.

| Change Case | PR | Status | Merge Status | Notes |
|---|---|---|---|---|
| ISSUE-201 | example/repo#3001 | Pending Manager Review | MERGED 2099-01-08 | Synthetic. |
| ISSUE-202 | example/repo#3002 | Pending Peer Review | MERGED 2099-01-08 | Synthetic. |
| ISSUE-203 | example/repo#3003 | Approved, Scheduled | MERGED 2099-01-09 | Synthetic. |
| ISSUE-204 | example/repo#3004 | Approved, Scheduled | MERGED 2099-01-09 | Synthetic. |
| ISSUE-205 | example/repo#3005 | New/Unassigned | MERGED 2099-01-10 | Synthetic. |
| ISSUE-206 | example/repo#3006 | Approved, Scheduled | MERGED 2099-01-10 | Synthetic. |

### Active cases needing manual review (no PR URL)

| Change Case | Subject | Status | Notes |
|---|---|---|---|
| ISSUE-301 | Synthetic case A | New/Unassigned | No PR URL; manual review. |
| ISSUE-302 | Synthetic case B | Approved, Scheduled | No PR URL; manual review. |

### Closed Change Cases

| Change Case | PR | Closed |
|---|---|---|
| ISSUE-401 | example/repo#4001 | 2099-01-11 |
| ISSUE-402 | example/repo#4002 | 2099-01-11 |
| ISSUE-403 | example/repo#4003 | 2099-01-12 |
