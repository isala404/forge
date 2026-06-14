# Forge cookbook

Complete, copy-pasteable recipes for the patterns that show up in every app. Each page
uses the real Forge API; Rust is canonical, with notes where the Node/Python bindings
differ. For exact per-primitive semantics, see [`docs/contracts/`](../contracts/).

## Auth

- [Auth middleware: validating sessions and API keys](auth-middleware.md)
- [Session cookie setup](session-cookies.md)
- [API key authentication and rotation](api-key-auth.md)

## Requests

- [Emitting IETF RateLimit headers](rate-limit-headers.md)
- [Typed config and feature flags](typed-config-and-flags.md)

## Background work

- [Idempotent job handlers](idempotent-jobs.md)
- [Running the scheduler and maintenance loops](scheduler-and-maintenance.md)

## Storage

- [Direct upload/download with presigned blob URLs](direct-upload-download.md)

## Operations

- [Choosing backends: Postgres everywhere, filesystem blob](backends-and-config.md)
- [Deployment lifecycle recipes (web, workers, scheduler, multi-replica)](lifecycle-recipes.md)
