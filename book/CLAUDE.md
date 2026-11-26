# Book (mdBook, `mdbook build book`)

Agent guidance. This file is outside `book/src/` and is not part of the book.

- **The book is the ONE place of the documentation** (user decision 2026-11-26). `docs/` is
  gone; `README.md` keeps only an overview, the quick start and links into `book/src/`. Never
  copy a section into a second file: link it. **Not published** anywhere (no CI, no deploy, user
  decision 2026-11-26): readers use GitHub's Markdown view or `mdbook serve book`.
- **Structure** (`src/SUMMARY.md` decides the parts): Introduction; Getting Started
  (`getting_started/installation.md`, `first_start.md`, `cheat_sheet.md`); Using the Server
  (`concepts/concepts.md`, `guide/*.md`: databases, backups, models, access, security, web_ui,
  mcp, http_clients, service; `troubleshooting.md`); Reference (`reference/console.md`,
  `configuration.md`, `server_directory.md`, `http_routes.md`, `admin_api.md`,
  `limitations.md`); Project (`release_notes/<version>.md`, `release_notes/index.md`).
- **Page files stay where they are**: code comments, tests, `CLAUDE.md`, `openapi/server.json`
  and `release.yml` cite their paths (`book/src/reference/admin_api.md`, ...). Moving or renaming
  a page means fixing every citation (`git grep 'book/src/'`).
- **Every user-visible change updates the book in the same change**: a new console command or
  option goes into `cheat_sheet.md` (a copy-paste line) AND `reference/console.md` (the table);
  a new `server.toml`/`database.toml`/`mcp.toml`/`access.toml` key into
  `reference/configuration.md` (key, default, when read, meaning; verified in `config.rs` and
  friends); a new admin route into `reference/admin_api.md` (and `openapi/server.json`, checked
  by a test); a new runtime writer into the inventory of `reference/server_directory.md`
  "Concurrent changes"; a new error a user can hit into `troubleshooting.md` as a recipe; the
  change itself into `release_notes/<next version>.md`.
- **Release notes**: `release.yml` uses `book/src/release_notes/<version>.md` of the tagged
  version as the GitHub release text, so each version's page must exist before its tag. Links
  in a release page are relative to the book; GitHub shows them as text.
- **Style** (as Graphersal's book): plain English, short sentences, the reader's task first,
  then the details. Every command on its own line in a ```` ```bash ```` block, copy-pasteable,
  with the server directory `./srv` and the address `127.0.0.1:8484`; output in ```` ```text
  ````. Tables for keys, options and refusals. Facts come from the code (run the binary to copy
  real output); never invent a key, flag or message. A deliberate limit goes into
  `reference/limitations.md`.
- **Troubleshooting** is task recipes: a heading in the reader's words ("The port is in use"),
  what they see (the real message), then numbered or bulleted steps with commands.
- **Checks** before committing a book change:

  ```bash
  mdbook build book                       # no warnings; create-missing = false, so a SUMMARY entry without its file fails
  python3 scripts/check-book-links.py     # every relative link and #anchor resolves (against the built HTML)
  ```

- Never link `ai/` working files or the graphersal repository's `ai/` from a page.
