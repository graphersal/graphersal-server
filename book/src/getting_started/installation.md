# Installation

graphersal-server is one binary, `graphersal-server`. It needs no other program at run time.

There are prebuilt binaries for four platforms, from 0.3.0 on:

| Platform | Archive |
|---|---|
| Linux x86_64 (static, any distribution) | `graphersal-server-v<version>-linux-x86_64.tar.gz` |
| Linux aarch64 (static, any distribution) | `graphersal-server-v<version>-linux-aarch64.tar.gz` |
| macOS on Apple silicon | `graphersal-server-v<version>-macos-arm64.tar.gz` |
| Windows x86_64 | `graphersal-server-v<version>-windows-x86_64.zip` |

There is no prebuilt binary for macOS on Intel: build it there with Cargo
([With Cargo](#with-cargo)). Windows on Arm runs the x86_64 build.

## The installers

The installers find the archive of your platform in the release's `SHA256SUMS`, download it,
refuse it when the checksum does not match, and put the binary in place. They change nothing
else: no server directory, no service.

### Linux and macOS

```bash
curl -fsSL https://github.com/graphersal/graphersal-server/releases/latest/download/install.sh | sh
```

The binary goes to `~/.local/bin`. The installer does not change your `PATH`: it tells you when
that directory is missing from it. A given version or directory:

```bash
curl -fsSL https://github.com/graphersal/graphersal-server/releases/latest/download/install.sh | sh -s -- --version v0.3.1 --dir /usr/local/bin
```

On an Intel Mac the installer stops and names the Cargo command instead. A shell running under
Rosetta 2 on Apple silicon gets the native arm64 build.

### Windows (PowerShell)

```powershell
irm https://github.com/graphersal/graphersal-server/releases/latest/download/install.ps1 | iex
```

The binary goes to `%LOCALAPPDATA%\Programs\graphersal-server`. The installer adds that directory
to your user `PATH` (the registry's `HKCU\Environment`; its other entries stay as they are) and to
the current window's, so `graphersal-server` runs there and in every new window. A given version
or directory, without touching `PATH`:

```powershell
& ([scriptblock]::Create((irm https://github.com/graphersal/graphersal-server/releases/latest/download/install.ps1))) -Version v0.3.1 -Dir C:\Tools -NoPathUpdate
```

### Options

| `install.sh` | `install.ps1` | Environment variable | Default |
|---|---|---|---|
| `--version vX.Y.Z` | `-Version vX.Y.Z` | `GRAPHERSAL_SERVER_VERSION` | the latest release |
| `--dir DIR` | `-Dir DIR` | `GRAPHERSAL_SERVER_INSTALL_DIR` | `~/.local/bin`; Windows `%LOCALAPPDATA%\Programs\graphersal-server` |
| `--base-url URL` | `-BaseUrl URL` | `GRAPHERSAL_SERVER_BASE_URL` | `https://github.com/graphersal/graphersal-server/releases` |
| | `-NoPathUpdate` | `GRAPHERSAL_SERVER_NO_PATH_UPDATE=1` | `PATH` is updated |

`--base-url` points at a copy of the releases (it holds `latest/download/<file>` and
`download/<tag>/<file>`, as GitHub does), for a mirror or a test. `install.sh --help` lists the
options.

### Upgrading

Run the installer again. A running server keeps the old version until it is restarted:

```bash
graphersal-server stop --dir ./srv
graphersal-server start --dir ./srv
```

On Windows a running `graphersal-server.exe` cannot be overwritten: the installer renames it to
`graphersal-server.old.exe` and puts the new one in place; the old file is removed by the next
install once it is free.

## With Cargo

From crates.io, built from source (Rust 1.88 or newer, <https://rustup.rs>, and a C compiler for
the `ring` crate):

```bash
cargo install --locked graphersal-server
```

With [cargo-binstall](https://github.com/cargo-bins/cargo-binstall), the release's prebuilt
binary goes into Cargo's `bin` directory, without building. On a platform without a prebuilt
binary (macOS on Intel included) it builds from source like `cargo install`:

```bash
cargo binstall graphersal-server
```

## Manual download and verification

Every [GitHub Release](https://github.com/graphersal/graphersal-server/releases) carries the four
archives, the installers `install.sh` and `install.ps1`, and `SHA256SUMS` over all of them. Each
archive holds the binary with `LICENSE-APACHE`, `NOTICE`, `README.md` and
`THIRD-PARTY-LICENSES.html`, in a directory named like the archive.

1. Download the archive of your platform and `SHA256SUMS`.
2. Check the checksum. Linux:

   ```bash
   sha256sum --check --ignore-missing SHA256SUMS
   ```

   macOS:

   ```bash
   shasum -a 256 --check --ignore-missing SHA256SUMS
   ```

   Windows (PowerShell): compare the output with the archive's line in `SHA256SUMS`:

   ```powershell
   (Get-FileHash graphersal-server-v0.3.1-windows-x86_64.zip -Algorithm SHA256).Hash.ToLower()
   ```

3. Verify the build provenance. The release workflow attests every archive and installer on
   GitHub; check it with the [GitHub CLI](https://cli.github.com/):

   ```bash
   gh attestation verify graphersal-server-v0.3.1-linux-x86_64.tar.gz -R graphersal/graphersal-server
   ```

4. Unpack it and put `graphersal-server` (Windows: `graphersal-server.exe`) on your `PATH`:

   ```bash
   tar -xzf graphersal-server-v0.3.1-linux-x86_64.tar.gz
   sudo install -m 755 graphersal-server-v0.3.1-linux-x86_64/graphersal-server /usr/local/bin/
   ```

The Linux binaries are static (musl): they run on every distribution, with no system library.

## From a clone

You need Rust 1.88 or newer (<https://rustup.rs>) and a C compiler (for the `ring` crate).

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

## A Docker image

The repository's `Dockerfile` builds the server into an image of its own (no image is published
yet): [Running in Docker](../guide/docker.md).

## Check it

```bash
graphersal-server --version
graphersal-server --help
```

`--help` prints every command with its options. The [Console Commands](../reference/console.md)
page has the same in a table, and the [Command Cheat Sheet](../cheat_sheet.md) as one-liners.

On Windows, Ctrl+C (or Ctrl+Break) stops a server running in the console cleanly.

Next: [First Start](first_start.md).
