# Issue Tracker Loop (synthetic fixture)

Synthetic progress.md exercising the markdown extractors in
`src/metrics/`. The example loop tracks generic "issue cases" against
PRs; nothing here is real.

## Objective

Demonstrate the resume-prompt + tracker schema for a long-running
issue-tracker loop:
- monitor cases created via a fictional `pr-to-issue` workflow,
- remind reviewers when cases stall in review,
- auto-close approved cases once the underlying PR is merged.

## Key Rules

- Never close a case if its PR has not been merged.
- Cases stuck >24h in `Pending Manager Review` get a reminder DM.
- All notifications append a synthetic disclaimer.

## Tools & Scripts

- `python3 skill/scripts/tick.py` — refresh cache + tracker, emit
  `work.json` describing what changed (synthetic — does not exist).

## Cache File Structure

Cache files at `~/.cache/example-issue-tracker/<case-id>.json`. Schema
is illustrative only.

## Notification Channels

- `#issue-tracker-team` — peer review notifications (synthetic name).

## Tracker File

Updated at `tracker.md` alongside this `progress.md`.

## Loop Configuration

Defaults: every 10 minutes, dispatch = `tick_script`. Override per-loop
in `workbench.toml`.
