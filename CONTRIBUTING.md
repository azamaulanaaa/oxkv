# Contributing

## Install the hooks

Run:

```bash
mise run hooks
```

This command sets `core.hooksPath` to `.githooks`. It also makes the hook scripts executable.

## Pre-commit hook

The `pre-commit` hook runs when you commit. It looks for staged Rust files.

- If no staged file ends in `.rs`, the hook exits 0.
- If a staged file ends in `.rs`, the hook runs `cargo fmt --check`.

The hook stops the commit when the format check fails.

## Pre-push hook

The `pre-push` hook runs before you push. It ignores branch deletions. If the push has at least one ref, the hook runs the default local CI jobs. The hook stops the push when a job fails.

## Run local CI

Run one job:

```bash
.githooks/ci-local.sh native
```

Run more than one job:

```bash
.githooks/ci-local.sh native wasm
```

Run every job:

```bash
.githooks/ci-local.sh --all
```

The default jobs are `native`, `wasm`, and `release-config`. The `wasm-browser` job runs only with `--all` or when you name it.

List the job names:

```bash
.githooks/ci-local.sh --list
```

Show the help text:

```bash
.githooks/ci-local.sh --help
```

The script stops at the first failing job. It returns the exit code of that job. The script does not download toolchains or targets. If a required tool is missing, the script skips the job and prints the install command.

## Strict mode

Set `CI_LOCAL_STRICT=1` to turn each skipped job into a failure:

```bash
CI_LOCAL_STRICT=1 .githooks/ci-local.sh --all
```

A skipped job can hide a problem. Use strict mode when your machine must run the full CI.

## Skip the hooks

Set `CI_LOCAL_SKIP=1` to make the hooks exit 0 with a message:

```bash
CI_LOCAL_SKIP=1 git commit
CI_LOCAL_SKIP=1 git push
```

You can also bypass the `pre-push` hook:

```bash
git push --no-verify
```

Use these options only for a temporary or local-only change.

## GitHub CI

The local hooks do not replace GitHub CI. GitHub CI uses a clean container and runs all jobs. A local run can skip a job when a tool is missing. Push your branch and confirm that GitHub CI passes.
