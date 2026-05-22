The `opentelemetry` sink's `grpc` protocol now supports `local_credential`, which fetches
short-lived credentials from a local HTTP issuer and attaches them as gRPC metadata on each
export.

authors: sakateka
