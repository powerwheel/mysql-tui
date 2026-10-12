# mysql_tui

> A fast, keyboard-driven MySQL client for your terminal — written in Rust with [ratatui](https://github.com/ratatui/ratatui).

[![Rust](https://img.shields.io/badge/rust-2024_edition-orange.svg)](https://www.rust-lang.org)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-linux%20%7C%20macos%20%7C%20windows-lightgrey.svg)](#installation)

[简体中文](README.zh-CN.md)

`mysql_tui` is for people who live in the terminal and want more than the stock `mysql` shell, without reaching for a heavyweight GUI. One static binary, no runtime, no Electron: connection manager, schema-aware autocompletion, multi-tab editor, scrollable result grid, streaming export and SSH/TLS connectivity — all driven from the keyboard.

```
┌ mysql_tui ──────────────────────────────── root@127.0.0.1:3306/shop  MySQL 8.4.3 ─┐
│ 1:SELECT * FROM orders  2:users ⟳            Ctrl+T new  Ctrl+W close  Alt+←/→ switch │
├ Databases ─────┬ Editor ───────────────────────────────────────────────────────────┤
│ ▾ shop         │ SELECT o.id, o.status, u.email, o.total                             │
│   ▸ customers  │ FROM orders o JOIN users u ON u.id = o.user_id                      │
│   ▾ orders     │ WHERE o.created_at > '2026-01-01'                                   │
│       id       │                                                                     │
│       user_id  ├ Results - 1000 rows x 4 cols | n: next 1000 ─────────────────────────┤
│       status   │ id      status     email                   total                    │
│       total    │ 10231   paid       alice@example.com       129.90                   │
│   ▸ users      │ 10232   shipped    bob@example.com         48.00                    │
│ ▸ analytics    │ 10233   pending    carol@example.com       310.25                   │
│ ▸ information… │ …                                                                   │
├────────────────┴─────────────────────────────────────────────────────────────────────┤
│ 1000 rows in 0.041s     Results | n next batch | Enter view cell | y/Y copy | F1 help │
└──────────────────────────────────────────────────────────────────────────────────────┘
```

## Highlights

- **Connection manager** — save multiple servers, auto-connect on launch, encrypted at rest (AES-256-GCM, Argon2id-derived key) behind a master password.
- **Secure connectivity** — TLS with `required` / `verify-ca` / `verify-identity` modes and custom CA; SSH tunnels through a jump host using your system `ssh` (agent & key auth).
- **Smart editor** — multi-line editing, undo/redo, selection + clipboard, schema-aware completion of keywords, tables and columns, per-tab state with `Ctrl+T` tabs.
- **Result grid that scales** — column cursor, horizontal scrolling, multiple result sets per run, full-cell viewer with JSON pretty-print, and *load next batch* (`n`) instead of a hard row cap.
- **Streaming export** — CSV / TSV / JSON written page by page, so exporting a 50-million-row table does not need 50 million rows of RAM.
- **Safety rails** — automatic `LIMIT` on bare `SELECT`s, confirmation prompt before `DELETE` / `UPDATE` / `DROP` / `ALTER` / `TRUNCATE`, cancel a running query with `Esc` (server-side `KILL QUERY`).
- **Schema sidebar** — browse databases → tables → columns, `Enter` to `USE` a database or insert a table name, `F2` for `SHOW CREATE TABLE` and column details.
- **Query history** — searchable, persisted across sessions (`F3`).

## Installation

### From source

Requires Rust **1.88 or newer** (2024 edition, let-chains).

```bash
git clone https://github.com/powerwheel/mysql_tui.git
cd mysql_tui
cargo install --path .
```

Or run without installing:

```bash
cargo run --release
```

### System requirements

| Feature | Requirement |
| --- | --- |
| TLS connections | OpenSSL headers on Linux at build time (`libssl-dev` / `openssl-devel`); uses the platform TLS stack on macOS/Windows |
| SSH tunnels | An `ssh` client on `PATH`; authentication must work non-interactively (ssh-agent or key file) |
| Clipboard | X11 or Wayland session on Linux; works out of the box on macOS/Windows |

## Quick start

1. Launch `mysql_tui`.
2. **First run** — choose a master password. It encrypts `connections.toml`; you will be asked for it on every start. (Skipping is not an option: database passwords are never written to disk in plaintext.)
3. Press `n` to add a connection, fill in the form, `Enter` to save.
4. Select it and press `Enter` to connect. Press `a` to mark it for auto-connect.
5. Type SQL, press `F9`. Press `F1` any time for the key reference.

## Keyboard reference

### Connection list

| Key | Action |
| --- | --- |
| `Enter` | Connect |
| `n` / `e` / `d` | New / edit / delete connection |
| `a` | Toggle auto-connect for the selected entry |
| `Ctrl+Q` | Quit |

### Editor

| Key | Action |
| --- | --- |
| `F9` | Run the editor contents (multiple `;`-separated statements allowed) |
| `Enter` | New line |
| `Ctrl+Space` | Show completions · `Tab` accept · `Ctrl+P`/`Ctrl+N` navigate |
| `Ctrl+P` / `Ctrl+N` | Previous / next history entry (when no popup is open) |
| `Shift+arrows`, `Ctrl+A` | Select text / select all |
| `Ctrl+C` / `Ctrl+X` / `Ctrl+V` | Copy / cut / paste selection |
| `Ctrl+Z` / `Ctrl+Y` | Undo / redo |
| `Tab` | Jump to results pane |

### Results

| Key | Action |
| --- | --- |
| `j`/`k`, `↑`/`↓`, `PgUp`/`PgDn`, `Home`/`End` | Move row selection |
| `←` / `→` | Move column cursor (grid scrolls horizontally) · `Ctrl+Home` first column |
| `Enter` / `v` | Open the full-cell viewer |
| `n` | Load the next batch of rows |
| `y` / `Y` | Copy cell / copy row as TSV |
| `[` / `]` | Switch between result sets |
| `Tab` / `Esc` | Sidebar / back to editor |

### Cell viewer

| Key | Action |
| --- | --- |
| `j`/`k`, `PgUp`/`PgDn`, `g`/`G` | Scroll |
| `←`/`→` or `h`/`l` | Previous / next column |
| `J` / `K` | Next / previous row |
| `p` | Toggle pretty-printed JSON |
| `y` | Copy cell text |
| `Esc` / `q` / `Enter` | Close |

### Sidebar

| Key | Action |
| --- | --- |
| `j`/`k` | Move selection |
| `←` / `→` | Collapse / expand |
| `Enter` | `USE` database · insert table name into editor |
| `F2` | Table detail popup (`SHOW CREATE TABLE`, columns) |
| `Tab` / `Esc` | Back to editor |

### Tabs & global

| Key | Action |
| --- | --- |
| `Ctrl+T` / `Ctrl+W` | New / close query tab |
| `Alt+←`/`→`, `Ctrl+PgUp`/`PgDn`, `Alt+1..9` | Switch tabs |
| `F3` | Query history panel — type to filter, `Enter` to load, `Ctrl+D` to delete |
| `Ctrl+E` | Export current result set |
| `Ctrl+F` | Refresh schema cache |
| `Ctrl+D` | Disconnect |
| `Esc` | Cancel running query / close dialog |
| `F1` | Help |
| `Ctrl+Q` / `Ctrl+C` | Quit (`Ctrl+C` copies when the editor has a selection) |

## How the pieces work

### Row limits and "load next batch"

`mysql_tui` never pulls an unbounded result set into memory:

- A `SELECT` without a `LIMIT` gets `LIMIT <default_limit>` appended (default **1000**, see [Configuration](#configuration)). Set `default_limit = 0` to disable.
- Regardless of `LIMIT`, at most **10 000** rows per result set are kept in the grid.
- When more rows may exist, the results title shows `| n: next <page>` — press `n` to append the next page. Paging reuses your statement with `LIMIT/OFFSET`, so it works for `SELECT` and `WITH … SELECT` and preserves an existing `LIMIT o, n` / `LIMIT n OFFSET o` window.
- Need everything? `Ctrl+E` → *export all* streams the full result to disk.

### Export

`Ctrl+E` opens the export dialog (path + CSV/TSV/JSON). For complete, non-truncated results the in-memory rows are written. For truncated results the original `SELECT` is re-run in pages of 10 000 rows and appended to the file as it goes, with live progress in the status bar. `~` is expanded and the matching extension is added when missing.

### Destructive-statement guard

Any statement starting with `DELETE`, `UPDATE`, `DROP`, `ALTER` or `TRUNCATE` shows a confirmation popup before it is sent. A `WHERE` clause does not bypass the prompt — it is deliberately simple.

### Multi-statement runs

Separate statements with `;`. Every statement that returns columns becomes its own result set (`[` / `]` to switch); statements that only affect rows are summarised in the status bar. DDL and `USE` trigger an automatic schema refresh so completion stays current.

### SSH tunnel

Fill in **SSH Host** (plus optional port, user, key path). `mysql_tui` runs `ssh -N -L 127.0.0.1:<random>:<db-host>:<db-port> …` as a child process and connects MySQL through the local port. Anything that works with a manual `ssh -L` works here; password prompts do not (the child runs with no TTY). The tunnel is torn down on disconnect.

### TLS

| Mode | Behaviour |
| --- | --- |
| `off` | Plaintext |
| `required` | Encrypt; accept any certificate |
| `verify-ca` | Encrypt; verify the chain against the system store or **TLS CA** |
| `verify-identity` | As `verify-ca` plus hostname check (skipped automatically through an SSH tunnel, where the peer is always `127.0.0.1`) |

If a TLS mode is set and the server does not negotiate TLS, the connection is refused rather than silently downgraded. The negotiated cipher is shown in the header.

## Configuration

Files live in your platform config directory (`~/.config/mysql-tui/` on Linux, `~/Library/Application Support/mysql-tui/` on macOS, `%APPDATA%\mysql-tui\` on Windows):

| File | Contents |
| --- | --- |
| `connections.toml` | Connections and settings — **encrypted** with your master password (AES-256-GCM, key derived with Argon2id, random salt + nonce) |
| `history.json` | Last 200 executed queries, plaintext |

Settings that are edited through the UI are stored inside the encrypted file; the decrypted structure looks like:

```toml
default_limit = 1000      # LIMIT appended to bare SELECTs; 0 disables
auto_connect = 1          # 1-based index into [[connections]]; omit to disable

[[connections]]
name = "local"
host = "127.0.0.1"
port = 3306
user = "root"
password = "..."
database = "shop"

[connections.tls]
mode = "verify-ca"        # off | required | verify-ca | verify-identity
ca_path = "~/certs/ca.pem"

[connections.ssh]         # optional
host = "bastion.example.com"
port = 22
user = "deploy"
key_path = "~/.ssh/id_ed25519"
```

Forgot the master password? There is no recovery — delete `connections.toml` and re-enter your connections.

## Architecture in one paragraph

A single background **DB worker thread** owns the MySQL connection and talks to the UI over `mpsc` channels (`DbCommand` in, `DbEvent` out), so slow queries never freeze rendering. The UI loop (crossterm + ratatui) polls events, keeps per-tab editor/result state, and routes asynchronous results back to the tab that issued them. Cancelling opens a throwaway connection to run `KILL QUERY <id>` against the worker's session. Exports and "load next batch" are the same worker running `LIMIT/OFFSET` pages. Everything is plain Rust with no `unsafe`.

```
src/
├── main.rs        entry point
├── app.rs         state machine, key handling, tabs, db-event routing
├── ui.rs          all ratatui rendering
├── db.rs          worker thread, connect/TLS/SSH, query & paging, KILL QUERY
├── editor.rs      text buffer with undo/redo and selection
├── completer.rs   keyword/table/column completion
├── schema.rs      cached schema metadata
├── exporter.rs    CSV/TSV/JSON writers (streaming)
├── history.rs     persisted query history
├── config.rs      connections.toml model and (de)serialisation
└── crypto.rs      AES-256-GCM + Argon2id for the config file
```

## Development

```bash
cargo test                      # unit tests (headless UI tests use ratatui's TestBackend)
cargo clippy --all-targets      # lint
cargo build --release           # LTO-optimised binary in target/release/mysql_tui
```

Tests do not need a database. A few `#[ignore]`d probes exercise a real local server when one is available:

```bash
cargo test -- --ignored
```

## Roadmap

- Transaction mode (`BEGIN` … `COMMIT`/`ROLLBACK` toggle with visible state)
- Query result diffing between tabs
- MariaDB-specific syntax in the completer
- Mouse support

Ideas and bug reports are welcome — please open an issue.

## Contributing

1. Fork and create a branch.
2. `cargo fmt`, `cargo clippy --all-targets`, `cargo test` must be clean.
3. Add tests for behaviour changes — app-level tests drive `App` with synthetic `KeyEvent`s and `DbEvent`s and are cheap to write.
4. Open a pull request describing *why* as well as *what*.

## License

MIT — see [LICENSE](LICENSE).
