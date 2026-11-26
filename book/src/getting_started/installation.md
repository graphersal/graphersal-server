# Installation

graphersal-server is one binary, `graphersal-server`. It needs no other program at run time.

## Windows x86_64: the prebuilt binary

Each release on GitHub carries a prebuilt binary:

- `graphersal-server-v<version>-windows-x86_64.zip`: `graphersal-server.exe` with
  `LICENSE-APACHE`, `NOTICE`, `README.md` and `THIRD-PARTY-LICENSES.html`;
- `SHA256SUMS`: the checksums to verify it.

Verify the download in a console:

```text
certutil -hashfile graphersal-server-v<version>-windows-x86_64.zip SHA256
```

Or in PowerShell:

```text
Get-FileHash graphersal-server-v<version>-windows-x86_64.zip
```

Unpack it and run `graphersal-server.exe` from a console. Ctrl+C (or Ctrl+Break) stops a
server cleanly.

## Every other platform: build from source

Linux, macOS and the rest build from source. You need Rust 1.88 or newer
(<https://rustup.rs>).

```bash
git clone https://github.com/graphersal/graphersal-server.git
cd graphersal-server
cargo build --release --locked
```

The binary is `target/release/graphersal-server`. Copy it onto your `PATH`:

```bash
sudo install -m 755 target/release/graphersal-server /usr/local/bin/
```

The profile of the release binaries adds fat LTO and strips the symbols. It takes longer to
build:

```bash
cargo build --profile dist --locked
```

Its binary is `target/dist/graphersal-server`.

`--locked` builds exactly the versions in the committed `Cargo.lock`. The Graphersal crates
come from crates.io, pinned to one exact version in the root `Cargo.toml`. The published
`graphersal-serve` crate carries the playground, so the binary has the whole web UI built in.

## Check it

```bash
graphersal-server --version
graphersal-server --help
```

`--help` prints every command with its options. The [Console Commands](../reference/console.md)
page has the same in a table, and the [Command Cheat Sheet](../cheat_sheet.md) as one-liners.

Next: [First Start](first_start.md).
