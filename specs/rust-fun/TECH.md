# Rust Fun MVP technical blueprint

**Status:** Phase 2 implementation  
**Scope:** Phase 0 foundation, Phase 1 local CLI/HTTP MVP, and Phase 2 local development workflows  
**Source:** User-provided “Rust Fun: Local Serverless Function Runtime — Architecture & Build Plan”

This document records the original Rust MVP and its Phase 2 development
workflow. Multi-runtime behavior added afterward is specified in
[`specs/rust-fun-runtimes/PRODUCT.md`](../rust-fun-runtimes/PRODUCT.md) and
[`specs/rust-fun-runtimes/TECH.md`](../rust-fun-runtimes/TECH.md).

## Context

The supplied brief describes a local-first serverless development runtime with Rust-native performance, a function contract independent of HTTP framework details, and optional later runtime adapters. The workspace had no implementation or Git metadata when planning began; this is a greenfield implementation, with no established conventions or checked-in `PRODUCT.md`.

The first release is intentionally narrower than the full roadmap: create a Rust function project, run registered functions locally, inspect them, and invoke them directly or over HTTP. Native functions execute as child processes with the operating-system privileges of the user running `fun`; process supervision is a lifecycle boundary, not a security sandbox.

## Proposed changes

### Workspace and ownership

Use the three-crate workspace from the brief. Keep adapter and template sources optional and do not add WASM or external-runtime dependencies to the default build.

| Component | Owns |
|---|---|
| `crates/fun-core` | Framework-independent request/response and error types; protocol serialization; manifest parsing and validation; project/function discovery. |
| `crates/fun-server` | Axum/Tokio HTTP gateway, route matching, worker lifecycle and dispatch, request limits, graceful shutdown, and request tracing. Depends on `fun-core`. |
| `crates/fun-cli` | Clap commands, project-root selection, command-line overrides, user-facing diagnostics, and wiring into core/server. |
| `crates/fun-cli/src/templates/rust-http` | A minimal, self-contained Rust worker implementing the versioned wire protocol, embedded into the CLI binary and copied by `fun init`. |
| `tests/` | CLI, protocol/worker, HTTP, and security integration coverage using temporary projects and local processes only. |

`fun-core` owns the host-side contract and protocol types. Since this repository has no published SDK crate, `fun init` currently generates a self-contained worker with the same wire envelope rather than a repository-relative or unpublished `fun-core` dependency. The protocol is a compatibility boundary and must be tested end to end; a separately published SDK crate is deferred until the package identity and release process are established.

### Function and worker contract

Keep the public handler API independent of Axum and Tokio server types. The contract should contain:

- `FunctionRequest`: request ID, method, URL path, query pairs, path-parameter pairs, header pairs, and body bytes.
- `FunctionResponse`: status, header pairs, and body bytes.
- A structured `FunctionError` with a stable category and safe message; diagnostic details stay in worker stderr/logs rather than being returned to the caller.
- A `FunctionHandler` async trait for host-side adapters. The generated worker currently uses the equivalent async `handle` function with self-contained wire types until a separately published SDK crate exists.

Use a versioned JSON Lines protocol over each worker's stdin/stdout. Each line is one complete JSON envelope; body bytes are base64 encoded so arbitrary payloads round-trip. Include a protocol version and request ID in both request and response. Reserve stdout exclusively for protocol frames; worker diagnostics go to stderr. The initial worker handles one request at a time, avoiding response-correlation races. Do not retry an invocation whose outcome is unknown after a worker failure, because a function may already have performed side effects.

The host supervises one persistent child per discovered function. Start workers with a cleared environment and forward only explicit entries from the project-root `.env`; never implicitly forward parent environment variables. `.env` must be a regular file inside the project root. Serialize calls to that function's worker; calls to different functions may execute concurrently up to the configured global limit, and reject work immediately when the worker or global capacity is busy. On timeout, malformed protocol output, or child exit, kill and reap the process, return a classified error for the affected request, and allow a later request to start a fresh worker. Do not automatically replay the failed request. Reject unsupported protocol versions explicitly.

Build each function with Cargo, honoring `CARGO_TARGET_DIR` if set (otherwise use `<project>/.rust-fun/target`), then stage its executable under `<project>/.rust-fun/bin/<function>/`. Staging keeps workers with identical Cargo binary names distinct and gives the supervisor a stable executable to restart.

### Project manifest, discovery, and routing

Define `rust-fun.toml` as the sole required project configuration for the MVP. Suggested schema:

```toml
[project]
name = "my-functions"
version = "0.1.0"

[dev]
host = "127.0.0.1"
port = 3000
watch = true

[functions]
directory = "functions"
default_timeout_ms = 5000
max_concurrency = 64
max_request_body_bytes = 1048576
max_response_body_bytes = 1048576

[[routes]]
path = "/api/hello"
function = "hello"
methods = ["POST"]
```

Route entries may include `methods`; when omitted, they match any method. The generated sample route explicitly accepts `POST`. Use `{name}` path parameters (for example `/api/users/{id}`), pass extracted values in `FunctionRequest`, and preserve query values separately from the path. Reject duplicate or overlapping route/method definitions during startup, including overlapping parameter patterns. Return 404 for no matching path and 405 with an `Allow` header when the path matches but the method does not.

For the first version, discover function directories beneath the configured functions directory and require each discovered function to build to the expected worker executable when `fun dev` starts. `fun list` must not build or execute workers. Validate function names, manifests, ports, timeouts, concurrency, body limits, and route targets before serving. Cap concurrency at 4096 and request/response body limits at 64 MiB. Reject unknown manifest fields so typos do not silently change behavior. Canonicalize project and function paths and reject paths or symlinks that escape the project root.

Configuration precedence is command-line override, then manifest, then documented default. `fun dev` exposes `--host`, `--port`, `--watch`, and `--no-watch` overrides, subject to loopback-only validation. Bind only to a loopback IP by default and reject non-loopback hosts in the MVP. Do not silently fall back to a public interface.

### CLI and request behavior

| Command | MVP behavior |
|---|---|
| `fun init [path]` | Create the manifest, `.env.example`, a `.gitignore` that excludes secrets and generated artifacts, a compilable Rust HTTP worker template, and a sample POST route. Refuse to overwrite a non-empty project; no overwrite option is provided. |
| `fun dev` | Validate the project, load `.env`, build required workers, start workers and the HTTP server, report the loopback URL, log requests with a request ID, and shut down cleanly on termination. Debounced watch mode rebuilds only changed functions; a failed rebuild leaves the last working worker active. Changes to `rust-fun.toml` require restart. `.env` changes restart workers without changing the parent process environment. `--no-watch` disables watching. |
| `fun list` | Print discovered functions and their configured routes in deterministic order without building or executing workers; fail with actionable diagnostics for invalid configuration or discovery metadata. |
| `fun test` | Run `cargo test` in each discovered function, stopping on the first failure and using the configured Cargo target directory. |
| `fun invoke <function> [--json <value>] [--method <method>] [--path <path>]` | Invoke a named function through the same worker protocol as HTTP. Default to `POST /`; JSON input is encoded as the request body. Output response JSON when valid, otherwise a clear byte/text representation. Report function errors on stderr and return a nonzero exit status. |

Reserve `GET /__fun/health` for a local liveness response (`{"status":"ok"}`); this does not assert that every worker is healthy. Watch events are debounced for 300 ms, ignore runtime artifact paths, and classify changes to a function by its discovered directory. File changes are applied after rebuilding and starting a replacement worker; build/start failures are logged and leave the current process serving.

Apply the global concurrency limit before dispatch and fail fast with 503 when saturated rather than accumulating an unbounded request queue. Enforce request and response body limits before forwarding/writing bodies. The invocation timeout includes worker restart and handler response. On graceful shutdown, stop accepting requests, drain in-flight work for at most 10 seconds, then terminate and reap remaining children.

Map failures consistently: invalid project/configuration prevents startup; unmatched route is 404; method mismatch is 405; saturated capacity is 503; worker timeout is 504; worker crash, invalid response, or protocol mismatch is 502; function-declared invalid request is 400 and other function errors are 500. Return safe error bodies to HTTP clients and retain detailed causes only in structured local logs. Validate response status and headers before writing them to the client.

## Testing and validation

The implementation must prove the Phase 1 and Phase 2 outcomes from the supplied brief without cloud services or Docker. Building the generated Rust project uses Cargo and may resolve crates through the configured registry:

1. `cargo check --workspace`, `cargo test --workspace`, formatting, and lint checks pass.
2. `fun init` inside the repository workspace creates a nested standalone project whose worker builds, lists, invokes, and runs tests.
3. HTTP integration tests cover sample route execution, method mismatch, response headers, request-size limits, worker errors, recovery after an error, health response, `.env` loading/reload, source reload, and response body/status; core route tests cover path parameters and safe `.env` loading.
4. Configuration tests cover defaults and CLI overrides, invalid ports/limits, unknown fields, missing functions, overlapping routes, and non-loopback binding rejection.
5. Worker tests cover valid protocol exchange, function-error frames, malformed frames, response validation, timeout/termination, and recovery after a malformed response. Verify the failed request is not replayed.
6. Limit tests assert oversized request and worker response bodies are rejected at their respective boundaries, and saturated concurrency returns 503; assert the handler is not called when the request is rejected before dispatch.
7. Path tests cover traversal and symlink escape attempts, including links inside function source trees. Verify child processes are reaped after both normal shutdown and failure.
8. CLI tests assert stable, actionable stderr and nonzero exit status for invalid projects and function failures.

Use temporary directories, ephemeral loopback ports, and purpose-built fixture workers. The end-to-end template test must build the generated project using its declared dependencies and verify host/template protocol compatibility.

## Ordered implementation tasks

1. **Workspace and CI foundation** — Add the Cargo workspace, the three crate skeletons, toolchain/format configuration, and basic CI checks. Acceptance: clean workspace check and formatting succeed.
2. **Core contract and template** — Implement framework-independent types and versioned protocol, then create the sample worker. Keep the generated project self-contained until the `fun-core` package has a release strategy. Acceptance: host/worker protocol round-trip tests pass and the template builds without repository-relative paths.
3. **Manifest and discovery** — Parse/validate TOML, resolve project root, discover functions, validate routes and safe paths. Acceptance: valid sample project is discovered; invalid and escaping configurations fail before execution.
4. **Worker supervisor** — Build/start persistent workers, serialize per-function calls, enforce global concurrency and timeout, classify failures, and reliably kill/reap children. Acceptance: protocol, timeout, crash, saturation, and cleanup tests pass.
5. **HTTP gateway** — Implement loopback-only Axum server, route/method/path-parameter matching, request/response conversion, body limits, error mapping, and graceful shutdown. Acceptance: HTTP integration tests cover successful and failure paths.
6. **CLI workflows** — Implement `init`, `dev`, `list`, and `invoke` over the core/server APIs, including readable errors and deterministic output. Acceptance: temporary-project end-to-end CLI test creates and invokes the sample function.
7. **Release gate and documentation** — Run the acceptance suite, document configuration and native execution limitations, and verify generated projects build using the selected SDK distribution method. Acceptance: all listed checks pass; no claim of native sandboxing or unimplemented watch behavior.

Tasks 2–6 depend on earlier contracts and workspace setup; do not parallelize contract, worker, and HTTP implementation before their interfaces stabilize.

## Risks and mitigations

- **Native privilege boundary:** Cargo build scripts and worker processes run with the user's privileges. State this plainly in CLI/docs; only accept trusted local code. WASM or OS-level isolation is a separate phase.
- **Protocol compatibility:** host and generated worker can be upgraded independently. Version every frame and fail clearly on unsupported versions; test the supported-version matrix.
- **Worker serialization:** one worker per function serializes same-function requests. Document this MVP limit; consider a configurable worker pool only after measurement.
- **Protocol duplication:** the generated project embeds the wire contract until a separately distributed SDK exists. Keep the host and generated-worker protocol fixtures/tests aligned; publishing an SDK is follow-up work.
- **Platform process semantics:** termination and child cleanup differ across operating systems. Target macOS/Linux initially and keep process control behind a small supervisor boundary so Windows support can be validated separately.
- **Watch reliability:** filesystem event streams vary by platform; debounce related events, surface rebuild errors, and preserve the last working worker until a replacement is ready.
- **Scope creep:** WASM, external runtimes, Lambda/Vercel compatibility, and deployment artifacts remain deferred.

## Parallelization

Do not split this MVP blueprint or initial contract work across agents. The workspace is empty, the product and protocol boundaries must be decided together, and later server/CLI work depends on those decisions. Parallel implementation would create integration churn before stable interfaces exist. Reassess after the contract, manifest, and worker protocol are implemented and covered by tests.
