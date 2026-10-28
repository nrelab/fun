# Rust Fun Runtime Support

## Summary

Rust Fun projects can bootstrap and execute functions written for the supported
Node.js, Python 3, Go 1.x, and provided runtimes, in addition to the existing
Rust runtime. Every runtime is invoked through the same Rust Fun request and
response contract, so routing and HTTP behavior remain consistent.

## Goals / Non-goals

- Keep Rust as the default for existing projects and `fun init` behavior.
- Let a project choose a starter runtime and discover functions of different
  runtimes side by side.
- Do not emulate AWS Lambda's Runtime API or claim compatibility with Lambda
  events, deployment packages, or hosted runtime images.

## Behavior

1. `fun init [path]` continues to create the existing Rust project by default.
   `fun init --runtime <runtime> [path]` creates an equivalent starter project
   for `nodejs`, `python3`, `go1.x`, or `provided`. An omitted path continues to
   use the existing default path.

2. The selected runtime is written into the generated function metadata.
   Runtime selection is per function, so one project may contain Rust and
   non-Rust functions. Existing Rust projects without runtime metadata continue
   to be discovered and executed as Rust.

3. Each generated starter contains its source, runtime metadata, and the
   runtime-specific files needed by `fun dev` to build or start it. The
   generated `provided` starter includes an executable `bootstrap` example.
   The generated Python starter includes at least one discoverable unittest so
   `fun test` verifies its handler instead of reporting an empty suite.
   `fun init` refuses to overwrite a non-empty destination, as it does today.

4. A function receives the same request fields regardless of runtime:
   request ID, method, path, query pairs, path parameters, headers, and body
   bytes. It returns an HTTP status, headers, and body bytes. The runtime
   adapter does not alter routes, query parsing, body-size limits, timeouts,
   or HTTP error mapping.

5. Node.js, Python 3, and Go 1.x starter functions expose the Rust Fun handler
   contract through their generated runtime entry point. Go functions are
   built before execution; Node.js and Python 3 functions are started with
   their installed system runtimes.

6. A `provided` function is started through its project-local `bootstrap`
   executable. The bootstrap must implement the documented Rust Fun worker
   protocol on stdin/stdout. Rust Fun does not infer a language or install a
   provided runtime on the user's behalf.

7. Runtime selection must be one of the supported identifiers. A missing
   required executable, malformed runtime metadata, a failed Go build, or a
   bootstrap that cannot start produces an actionable error naming the
   function and runtime; it must not silently fall back to Rust or another
   runtime.

8. `fun list` continues to avoid building or executing functions and displays
   each function's selected runtime. `fun invoke`, `fun dev`, health checks,
   watch reload, and worker supervision work the same across supported
   runtimes.

9. `fun test` runs the conventional test command for each selected runtime:
   `cargo test` for Rust, `node --test` for Node.js, `python3 -m unittest
   discover` for Python 3, and `go test ./...` for Go 1.x. A provided function
   has no assumed test runner; `fun test` reports that it is skipped with the
   function name and reason rather than reporting false success.

10. Runtime processes receive only the project `.env` values already supported
    by Rust Fun, not the invoking shell's complete environment. Worker stdout
    remains reserved for protocol frames; diagnostics go to stderr.

11. Runtime support requires the corresponding local executable: `node`,
    `python3`, or `go`. Rust functions continue to require Cargo and Rust as
    before. Missing tools are reported when the operation needs them, with an
    installation hint; listing functions does not require the tools.

12. Runtime names are stable manifest identifiers (`rust`, `nodejs`,
    `python3`, `go1.x`, and `provided`). Version-specific runtime selection,
    AWS Lambda compatibility, containerized runtimes, and sandboxing are
    outside this feature.
