# Rust Fun Runtime Support — Technical Plan

## Context

Rust Fun currently discovers only Cargo functions. [`DiscoveredFunction`](/Users/khulnasoft/Development/ƒun/crates/fun-core/src/discovery.rs) requires `Cargo.toml` and derives the binary name from its package metadata. [`WorkerPool::build_selected`](/Users/khulnasoft/Development/ƒun/crates/fun-server/src/runtime.rs) builds every function with Cargo and starts its executable as a persistent JSONL worker. The generated Rust worker in [`templates/rust-http/src/main.rs`](/Users/khulnasoft/Development/ƒun/crates/fun-cli/src/templates/rust-http/src/main.rs) demonstrates the versioned request/response frames. `fun init`, `fun test`, and `fun list` are implemented in [`crates/fun-cli/src/main.rs`](/Users/khulnasoft/Development/ƒun/crates/fun-cli/src/main.rs). The Phase 2 integration test exercises generated project creation, worker invocation, HTTP routing, watch reload, and `.env` reload in [`tests/end_to_end.rs`](/Users/khulnasoft/Development/ƒun/crates/fun-cli/tests/end_to_end.rs).

The feature adds language workers while retaining the existing process and
protocol boundary. It does not implement AWS Lambda's Runtime API: that would
change event semantics, bootstrap lifecycle, and response mapping beyond the
requested language support. Product behavior is defined in
[`PRODUCT.md`](./PRODUCT.md).

## Proposed changes

### Function metadata and discovery

- Add per-function `fun.toml` metadata containing `runtime` and optional
  `handler`. Define a typed, deny-unknown-fields configuration in `fun-core`.
- Supported identifiers are `rust` (the default for legacy directories),
  `nodejs`, `python3`, `go1.x`, and `provided`.
- Keep current Rust discovery behavior for a directory containing `Cargo.toml`
  and no `fun.toml`. When `fun.toml` exists, use its runtime and validate the
  files required by that runtime. Reject ambiguous directories that declare
  conflicting runtime manifests.
- Preserve `DiscoveredFunction`'s stable name and working-directory fields;
  add runtime-specific metadata rather than assuming every function has a
  Cargo package name.
- Add `runtime` to `fun list` output without building or probing executables.

### Runtime worker abstraction

- Replace Cargo-specific build/start logic in `WorkerPool` with a small
  runtime-owned build/launch descriptor. The descriptor identifies an optional
  build operation, executable, arguments, working directory, and worker
  environment. Keep the existing JSONL request protocol, per-function
  serialization, timeouts, limits, and restart/reload semantics unchanged.
- Rust remains Cargo build + staged binary. Go uses `go build` to produce a
  staged executable. Node.js starts `node` with its generated entry script;
  Python starts `python3` with its generated entry script. `provided` starts
  `<function>/bootstrap` directly and does not run a build command.
- Resolve tool executables through the host `PATH`; do not forward the host
  environment to workers. Forward only `.env` entries while setting required
  runtime metadata explicitly. Capture build output and return actionable
  errors. Keep diagnostics on stderr and protocol frames on stdout.
- Stage compiled binaries under `.rust-fun/bin/<function>/` with unique
  filenames, preserving the existing Rust behavior and avoiding collisions
  between projects or package names.

### Generated starter projects

- Extend `fun init` with a `--runtime` selector. Rust remains the default.
- Add self-contained templates for Node.js, Python 3, Go 1.x, and provided.
  Their handlers consume the same logical request fields and emit the existing
  versioned JSONL protocol. Keep each generated project independent of
  repository-relative dependencies.
- Generate executable permission for the `provided` bootstrap on Unix, and
  produce a clear diagnostic if the platform cannot execute the bootstrap.
- Update package `include` rules to include every embedded template asset.

### CLI and development workflow

- `fun test` dispatches the conventional per-runtime test command defined in
  `PRODUCT.md`; for `provided`, print an explicit skip diagnostic and continue.
- `fun dev`, `fun invoke`, and the watcher use the common worker descriptor.
  Watch reload rebuilds Go and Rust; interpreted runtimes restart from updated
  source. Retain the current worker until replacement startup succeeds.
- Validate the runtime for a function before serving, but defer checking
  external tool availability until the relevant build/start/test operation so
  `fun list` stays metadata-only.
- Update README with the supported runtime table, local tool requirements,
  handler contract, and non-Lambda compatibility boundary.

### Data flow

```text
function directory + fun.toml
          |
          v
 runtime descriptor -> optional build -> process spawn with .env only
          |                                  |
          +----------- JSONL worker ----------+
                             |
                    existing server/CLI dispatch
```

## Testing and validation

- Map PRODUCT behavior 1–3 to CLI tests for default Rust generation and each
  runtime template; verify destination refusal and generated file permissions.
- Map behavior 2, 7, and 8 to discovery/config tests for legacy Rust fallback,
  each runtime, invalid identifiers, missing required files, conflicting
  metadata, and deterministic `fun list` output without tool invocation.
- Map behavior 4–6 and 8 to integration fixtures that launch one worker per
  runtime through the shared JSONL protocol and assert identical request
  fields, response status/headers/body, invocation, and HTTP behavior.
- Map behavior 7 and 11 to tests for missing executables, failed Go compilation,
  worker start failure, and actionable diagnostics. Verify no runtime fallback
  occurs.
- Map behavior 9 to command-selection tests, including the explicit provided
  test skip.
- Map behavior 10 to environment-isolation tests: `.env` values are forwarded,
  parent-only variables are absent, stdout is protocol-only, and stderr
  diagnostics do not corrupt a response.
- Run `cargo fmt --all -- --check`, `cargo check --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace`. Verify `cargo package -p fun-cli --list` contains
  all embedded templates.
- When available, run generated Node.js, Python 3, and Go 1.x samples using
  their local runtimes. Tests must skip only when a corresponding tool is
  genuinely unavailable; CI should install/pin the tools needed for full
  runtime integration coverage.

## Parallelization

Do not parallelize initial implementation. Discovery metadata, worker launch
descriptors, CLI templates, and integration fixtures share the same runtime
contract; separating them before it stabilizes risks incompatible interfaces.
After the descriptor and manifest land, independent language templates can be
implemented in separate worktrees if needed, with one integration owner
responsible for protocol conformance and full-suite validation.

## Risks and mitigations

- **Protocol drift across languages:** include protocol fixtures and test every
  generated worker against the same host contract.
- **Untrusted native code:** all native workers and build tools retain the
  current user's privileges; document that runtimes are not sandboxes.
- **Platform differences:** executable permissions and Go output paths vary;
  keep launch-path construction platform-aware and test supported host OSes.
- **Misleading Lambda naming:** explicitly document that these identifiers
  select local runtimes and do not imply AWS Lambda Runtime API compatibility.
