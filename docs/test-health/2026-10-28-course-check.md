# Test health, 2026-10-28 (course check)

Baseline: `docs/test-health/2026-10-22.md` measured the final CI slow step
sum at 284.607 s (release build 192.643 s), under the 300 s budget.

What changed since:

- `tests/e2e` went from 109 to 1009 items for one release candidate when
  stress loops were committed as `parametrize(range(n))` on the fingerprint
  race tests. CI hit the 5-minute e2e timeout at 49% (exit 124, no test
  failed). The gate steps are unchanged; the suite just ran longer than
  the budget.
- The fix (cc13ca2) put the loop count behind `FPRINT_LOOPS`, default 3
  (2 per method for the flood tests). `pytest tests/e2e -n 4 --timeout 60`
  now runs 131 items in 107 s inside swayward-dev. A stress run with
  `FPRINT_LOOPS=400 -k fingerprint -n 16` does 3601 items in 7m19s and is
  run by hand, not in CI.

BEFORE/AFTER (e2e step only, as CI runs it):

| tree | items | wall | notes |
|---|---|---|---|
| 5a85936 | 109 | not measured | pre-existing budget |
| f016eb6 | 1009 | >5 m (timed out at 49%) | exit 124 |
| cc13ca2 | 131 | 107 s (4 workers, local) | green |

CI on PR #4 with the fix: `gate` 8m27s, `nix` 15m58s (those are whole-job
times, so they include the release build and packaging).

Rules, so this does not come back:

- Never commit a review- or stress-loop count (e.g. `parametrize(
  range(400))`) as the default. Put the count behind an env var with a
  small default and document the stress command in the test.
- A nondeterministic regression test only belongs in CI when it fails
  deterministically on the old code (the fingerprint flood-in-Claim test
  failed 50/50 on the parent). The ordering-race tests fail ~1.4% per run
  on the old code; they are smoke/regression checks now, and the docstring
  above them says so.
- Full suite item count and wall time belong in every PR that touches
  `tests/e2e`, next to the change. If the e2e step crosses the 5-minute
  budget on 4 workers, cut iterations, not coverage.

Review cost this week: the fingerprint unit needed 7 review rounds, the
secrets and cmd[]/Ctrl/setuid/grace units 2-3 each. Each round was a mu
task; the 4.0 mu database lost the early round count in the 3.x move, so
the rounds are not recoverable from mu alone. New rule: a review round is
a mu task (`review_x_2`, ...), so rounds survive, and `high` = fix +
re-review, `medium` = fix with evidence in the close note, `low` = one
batch. Same rule as the mu course-correct recipe uses.

Review is recorded as a mu task, so rounds survive, and the pull request
body carries one line:

```text
review: independent review, 2 rounds, 1 finding fixed (dead-key text under a locked Ctrl)
```

