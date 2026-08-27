# Version persisted contracts

Status: accepted
Date: 2026-08-27

## Context

Sessions, settings, authentication data, and extension messages outlive one
process invocation. Unmarked formats make a future reader unable to
distinguish old data from unsupported new data.

## Decision

New session headers and configuration writes carry explicit format versions.
Unmarked pre-release data remains readable as version 0. Extension protocol
versions remain independent from application and storage versions. Released
shapes are retained as fixtures.

## Consequences

Readers can migrate known old data and reject unknown future data safely.
Every incompatible format change must update its version, fixtures,
documentation, and migration behavior together.
