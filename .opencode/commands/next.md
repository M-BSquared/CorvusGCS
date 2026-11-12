---
description: Take the next open item of PLAN.md for an area and finish it end to end (implement, review, commit).
agent: orchestrator
---

Continue the Corvus GCS Rust program. Area or item (may be empty): $ARGUMENTS

Current state:

!`git log --oneline -8`
!`git status --short`

1. Read `AGENTS.md` and section 0 of `PLAN.md`. `PLAN.md` is the only list of open work. If an area was given, take its next P1 item, then P2; with no area, take the most valuable open P1. Say which item you took and why in one line.
2. Hand it to the agent that owns the files (the ownership table is in your own prompt and in each agent's `permission` block). Write the contract first when the change crosses areas, then fan out. Stay inside each area's files; anything else goes under NEXT.
3. Hold behaviour the Python program defined to the frozen fixtures in `src/core/tests/data` and `src/link/tests/data`. Never regenerate a fixture to match the code. No hardware: simulate everything, test against scripted vehicles.
4. A change that shows on screen gets a `verify` pass; a change to modes, parameters or `MAV_CMD`s is checked for PX4 v1.16 to v1.18 and ArduPilot; new interface strings go to `i18n`; user visible changes go to `doc` and `readme`.
5. Run `review`, then the gates (`/gates`). Remove the finished item from `PLAN.md` in the same commit, keeping the pinning test named.
6. One commit per finished module: subject line, then `*` bullets grouped by area. Stage files by name, never edit `VERSION` by hand, never push, no trailer.
7. Report in the orchestrator's short plain format: what changed, checks and results, what was not verified.
