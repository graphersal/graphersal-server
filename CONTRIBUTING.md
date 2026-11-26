# Contributing to graphersal-server

Thanks for your interest. graphersal-server is a multi-database HTTP and MCP server for the
[Graphersal](https://github.com/graphersal/graphersal) graph engine. It is EXPERIMENTAL and
changes quickly. The detailed rules and the architecture notes live in `CLAUDE.md` (written for
coding agents, readable by humans) and `book/CLAUDE.md` (the book); this file is the short human
version.

## Before you start

- **Bugs**: open an [issue](https://github.com/graphersal/graphersal-server/issues) with
  `graphersal-server --version`, your platform, the commands or HTTP requests that reproduce the
  problem, what you expected and what happened (the console's `Error:` and `Help:` lines, or the
  JSON answer). Leave out tokens, `access.toml` and real data.
- **A larger change** (a new command, route, file in the server directory or setting): open an
  issue first, so we can agree on the design before you write it.
- **Engine problems** (queries, the Store, the playground, the MCP tools themselves) belong to
  [Graphersal](https://github.com/graphersal/graphersal/issues), not to a workaround here.
- **Security issues**: never in public, see [SECURITY.md](SECURITY.md).

## Build and test

Rust 1.88 or newer and a C compiler (for the `ring` crate); Node.js (CI uses 24) for the web
UI's tests. The Graphersal crates come from crates.io, pinned to one exact version; `Cargo.lock` is
committed.

```bash
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
node --test crates/graphersal-server/ui/tests/*.test.mjs   # the web UI's modules
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --locked
cargo deny check                                          # cargo install --locked cargo-deny
```

The UI tests check the server's UI profile against the playground the binary embeds: the `ui/`
of the `graphersal-serve` crate Cargo downloaded (run a `cargo build` first), or a playground
directory named by `GRAPHERSAL_PLAYGROUND`.

The end-to-end smoke test builds the binary, creates a temporary server directory, serves it on
a free port and walks the main features (databases, backups, imports, MCP, users and grants, a
restart). It needs bash, curl and python3 (Linux, macOS); `scripts/smoke.ps1` is its port to
PowerShell 7 for Windows:

```bash
scripts/smoke.sh                                        # ok/FAIL line per step
pwsh scripts/smoke.ps1 -Bin target/debug/graphersal-server.exe
```

A change to the book (`book/`):

```bash
mdbook build book                       # no warnings
python3 scripts/check-book-links.py     # every relative link and #anchor resolves
```

CI does not run on its own: a maintainer starts it by hand for a pull request (Linux and
Windows, cargo-deny, the MSRV). Run the checks above locally before you open one.

## Rules (summary)

- No panics on request paths: no `unwrap`/`expect` outside tests; errors are `ServerError`
  values with a message that says what to do.
- Every user-visible change updates the book in the same pull request (a new command goes into
  `book/src/cheat_sheet.md` and `book/src/reference/console.md`, a new setting into
  `book/src/reference/configuration.md`, a new admin route into
  `book/src/reference/admin_api.md` and `openapi/server.json`) and the release notes of the next
  version (`book/src/release_notes/`). A new feature also gets a step in both smoke scripts.
- Tests never touch real data: temporary directories, synthetic data, servers on port 0.
- A new dependency only with a one-line justification in the root `Cargo.toml`.
- Library code logs through `tracing`, never `println!`.
- English in code, comments, docs and commit messages. Commit messages follow
  [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `fix:`, `docs:`,
  `test:`, `chore:`), signed off (`git commit -s`, a `Signed-off-by:` line) as the
  maintainer's are.

## Contributor License Agreement (CLA)

graphersal-server is licensed under Apache-2.0. Contributions are accepted under the
[Contributor License Agreement](CLA.md), the same agreement as Graphersal's: you keep the
copyright in your contribution and grant the maintainer a licence to use, distribute and
sublicense it, including under licence terms other than Apache-2.0, plus a patent licence for it.
This keeps the project able to change the licence of future versions; a version already
published under Apache-2.0 stays available under it.

You sign once. When you open your first pull request, the CLA check posts a comment with a link
to the agreement; reply with the comment

```text
I have read the CLA Document and I hereby sign the CLA
```

and the check turns green for this and every later pull request (comment `recheck` to run it
again). If you contribute on behalf of your employer, make sure you are allowed to accept the
agreement for it (section 1 of [CLA.md](CLA.md)). A pull request is merged only when every
author of its commits has signed.
