# Security Policy

## Reporting a vulnerability

Please do **not** open a public issue for security vulnerabilities.

Report privately via [GitHub Security Advisories](https://github.com/jonx/Ferail/security/advisories/new),
or by email to **code@jkn.me**.

Include enough detail to reproduce the issue (affected version or commit,
platform, and steps). You can expect an initial acknowledgement within a few
days. Once a fix is available, we will coordinate disclosure with you.

## Scope

Ferail is a desktop file manager. Of particular interest are issues where
untrusted filesystem content (file names, metadata, magic bytes, archive
contents, previews) can lead to crashes, path-traversal, code execution, or
unintended filesystem mutation.

## Supported versions

This project is pre-1.0. Only the latest `main` is supported; fixes land there.
