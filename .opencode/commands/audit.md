---
description: Audit the current diff (or a given commit range) against the AGENTS.md invariants; findings only, no fixes.
agent: review
subtask: true
---

Audit this change: $ARGUMENTS (empty means the working tree against HEAD).

!`git status --short`
!`git diff --stat`

Follow your own procedure: scope, gates, flight safety, both stacks, frozen fixtures, shutdown, bounds, version, security, offline, packaging, wording, docs. Report findings most severe first with `file:line`, what triggers them and the owner; then what you checked and what you did not. Write the missing regression tests you are allowed to write and say which.
