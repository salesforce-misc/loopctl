# Security policy

## Reporting a vulnerability

If you believe you have found a security vulnerability in `loopctl`,
please report it privately to **security@salesforce.com**.

Include in your report:

- A description of the issue and the code path that triggers it.
- A proof-of-concept or step-by-step reproduction, if you have one.
- The affected commit hash or release tag.
- Your name and how you would like to be credited (or to remain
  anonymous).

We will acknowledge receipt within five business days. Once we have
confirmed the issue, we will work with you on a fix and a coordinated
disclosure timeline. By default we ask that you withhold public
disclosure for up to **90 days** from the date of your report so we
have a window to ship the fix and notify downstream users.

## Out of scope

- Bugs that require physical access to a developer's machine.
- Misconfiguration of an end-user's `workbench.toml` (e.g. pointing
  `claude_loops_dir` at a directory the user does not own).
- Issues in third-party dependencies — please report those upstream
  first; we'll track and update.
