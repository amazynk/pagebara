# Security policy

Pagebara opens untrusted files, so parser and decoder bugs matter to us. Thank you for
reporting them responsibly.

## Reporting a vulnerability

Please **do not open a public issue**. Report it privately through
[GitHub's private vulnerability reporting](https://github.com/amazynk/pagebara/security/advisories/new),
or by email to [help@amazynk.com](mailto:help@amazynk.com) if you can't use GitHub.

Include the version, the platform, and a file or steps that reproduce the problem. If the
file contains personal data, make a minimal reproduction instead. We aim to acknowledge a
report within a week, keep you updated while we fix it, and credit you in the advisory
unless you prefer otherwise.

## Scope

In scope: crashes, hangs or excessive memory use on crafted files; redaction that leaves
content recoverable; signature verification that accepts a modified or forged document;
and anything that makes the app touch the network or the file system without the user
asking.

Out of scope: issues that need a malicious local user or a compromised operating system.

## Supported versions

Fixes go into the latest release. There are no long-term support branches yet.
