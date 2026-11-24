# Security policy

## Supported versions

Imanari is a 0.1 developer preview. Only the latest commit on `main` receives fixes.

## Reporting a vulnerability

Please do not open a public issue for a security problem. Report it through GitHub's private vulnerability reporting: go to the repository's **Security** tab and choose **Report a vulnerability**. Include the `.nr` source or request that triggers the problem, what you expected, and what happened.

You should get an acknowledgement within a week. Once a fix is ready, it is published with an advisory that credits the reporter, unless you ask not to be named.

## Scope

In scope:

- The compiler and generated code: injection into generated Rust, SQL, HTML or JavaScript, and authorization bypasses in generated actions.
- `imanari-runtime`: session, CSRF, Origin and Host checks, idempotency, revision checks, and request limits.
- Repair isolation: escaping the Seatbelt sandbox, edits outside the allowed scope, and changes to protected tests.
- Secrets leaking into incidents, context bundles or exported evidence.

Out of scope, as documented limits of 0.1 (see the README's *Limits and non-claims*):

- The `local_dev` identity against a hostile local user or malware on the same machine.
- Memory and process-count limits inside the macOS sandbox, which the OS does not enforce for this use.
- Vulnerabilities in dependencies, SQLite or the OS, unless Imanari uses them incorrectly.
