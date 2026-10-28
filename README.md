# Rust Fun

Rust Fun is a local-first development runtime for Rust, Node.js, Python 3, Go
1.x, and custom provided functions. Builds and native worker processes run with
the privileges of the current user; neither is sandboxed. Run only code you
trust.

## Build and run

```sh
cargo build --workspace
cargo install --path crates/fun-cli
cargo run -p fun-cli -- init example
cargo run -p fun-cli -- init --runtime nodejs example-node
cargo run -p fun-cli -- init --runtime python3 example-python
cargo run -p fun-cli -- init --runtime go1.x example-go
cargo run -p fun-cli -- init --runtime provided example-provided
cd example
fun dev
```

Or run the CLI directly from the workspace:

```sh
cargo run -p fun-cli -- init example
cd example
cargo run --manifest-path ../Cargo.toml -p fun-cli -- dev
```

In another terminal:

```sh
cargo run --manifest-path ../Cargo.toml -p fun-cli -- list
cargo run --manifest-path ../Cargo.toml -p fun-cli -- invoke hello --json '{"name":"Rust"}'
curl -H 'content-type: application/json' \
  -d '{"name":"Rust"}' http://127.0.0.1:3000/api/hello
fun test
```

The generated project is self-contained and builds/starts supervised workers
with the selected runtime (Rust honors `CARGO_TARGET_DIR` when set). Runtime
selection can also be mixed per function using a `fun.toml` file inside each
function directory. Rust projects created before runtime selection remain
supported. Node.js, Python 3, and Go functions require `node`, `python3`, and
`go` on the local machine; `provided` functions supply an executable
`bootstrap`. All functions use the same versioned Rust Fun JSONL worker
protocol. These identifiers select local language runtimes; Rust Fun does not
emulate the AWS Lambda Runtime API or Lambda event format. The generated
`.gitignore` excludes `.env`, build outputs, and runtime artifacts. Worker
stdout is reserved for the runtime protocol; use stderr for function diagnostics. The
server binds to loopback only, and workers start with a cleared environment in
this MVP except for entries in the project-root `.env` file. Copy
`.env.example` to `.env` to set local worker variables. Watch mode is enabled
by default and can be disabled with `fun dev --no-watch`. `fun test` runs each
runtime's conventional test command and reports provided functions as skipped.
The local health route is available at `GET /__fun/health`. Runtime isolation
and deployment adapters are not included yet.

See the [MVP technical blueprint](specs/rust-fun/TECH.md) and the
[runtime support specification](specs/rust-fun-runtimes/PRODUCT.md) /
[technical plan](specs/rust-fun-runtimes/TECH.md) for the manifest, worker
protocol, constraints, and implementation details.
