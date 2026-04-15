# rust-trx

Pretty-print test results in TRX format using a native Rust CLI.

This project ports the behavior of [`devlooped/dotnet-trx`](https://github.com/devlooped/dotnet-trx) to Rust.

Typical usage:

```bash
dotnet test --logger "trx"
rust-trx --output
```

It can also integrate with GitHub Actions to publish step summaries and PR comments (when running in CI with the expected GitHub environment variables and `gh` available).

## Features

- Discovers `*.trx` files from the current directory (or a custom path)
- De-duplicates results by `testId` across multiple TRX files
- Shows failed/skipped/passed tests based on verbosity
- Optional test output rendering (`--output`)
- Summary with pass/fail/skip counts and elapsed time
- Non-zero exit on failed tests (unless `--no-exit-code`)
- GitHub Actions reporting:
  - PR comment update/create
  - Step summary output

## Installation

### From source

```bash
git clone <your-fork-or-repo-url>
cd rust-trx
cargo install --path .
```

### Development run

```bash
cargo run -- --help
```

## Usage

```bash
rust-trx [OPTIONS]
```

## Options

```text
Pretty-print test results in TRX format

Usage: trx [OPTIONS]

Options:
  -p, --path <PATH>
  -o, --output
  -r, --recursive <RECURSIVE>    [default: true] [possible values: true, false]
  -v, --verbosity <VERBOSITY>    [default: quiet] [possible values: quiet, normal, verbose]
      --no-exit-code
      --gh-comment <GH_COMMENT>  [default: true] [possible values: true, false]
      --gh-summary <GH_SUMMARY>  [default: true] [possible values: true, false]
  -h, --help                     Print help
```

## CI/CD

GitHub Actions workflows are included to:

- run formatting/lint/tests on push and PR
- build release executables for multiple targets
- publish to crates.io (optional on dispatch)
- create a GitHub release with attached binaries

Version calculation in workflows uses [`git-versioner`](https://crates.io/crates/git-versioner) from Crown0815.

## Acknowledgements

- Original project and behavior reference: [`devlooped/dotnet-trx`](https://github.com/devlooped/dotnet-trx)
