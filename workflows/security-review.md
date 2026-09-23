---
agent: claude
---
Do a security review of this project, focused on what an attacker could actually exploit.

Check: handling of untrusted input, shell and SQL injection, path traversal, secrets committed to the repo or printed in logs, unsafe deserialization, auth and permission checks, and dependencies with known advisories.

Report each real issue with where it is, how it could be exploited, how serious it is, and the fix. Do not report theoretical issues with no realistic path. Do not change any files.
