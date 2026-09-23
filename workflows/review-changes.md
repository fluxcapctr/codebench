---
agent: claude
---
Review my uncommitted changes (git diff, plus untracked files) as a careful senior reviewer.

Look for real problems: bugs, crashes, wrong edge cases, data loss, security issues, and anything that breaks existing behavior. Skip style nitpicks.

For each finding give the file and line, what goes wrong and when, and a suggested fix. Rank them most serious first. If nothing is wrong, say so plainly.

Do not change any files.
