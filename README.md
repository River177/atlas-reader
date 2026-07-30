# Atlas Reader

Atlas Reader is a macOS bilingual academic PDF reader for Chinese researchers reading English
papers.

The product focuses on chapter-based English–Chinese close reading, preserving document structure,
formulas, citations, and page relationships. Cloud MinerU provides document parsing with a
user-supplied API key, while translation uses a user-configured OpenAI-compatible endpoint.

## Status

Atlas Reader is in foundation development. The repository contains a runnable Tauri 2 desktop shell,
a React and TypeScript frontend, Rust domain modules, SQLite migrations, generated TypeScript
contracts, and foundational tests.

Completed vertical slices:

1. **Local library import** — import PDFs from the native picker or drag and drop, search the local
   library, detect duplicates, refresh source status, relocate moved files, and remove records
   without deleting the original PDF.
2. **Original PDF reading** — open an imported paper in an embedded PDF.js viewer with page
   navigation, zoom, in-document search, and a reading position that survives closing the reader and
   restarting the application. The viewer reaches the PDF through a capability-token URL served by a
   dedicated `atlas-reader://` protocol, so absolute file paths never reach the frontend and a token
   stops working as soon as the source file changes or the reader closes.

The next vertical slice is automatic Cloud MinerU parsing with a user-supplied API key.

## MVP direction

- Lightweight local paper library
- Original PDF reading
- Automatic Cloud MinerU parsing with a user-supplied API key
- Chapter-based bilingual reading
- Formula, citation, and block-structure preservation
- Inline explanation, retranslation, and preferred wording
- Local caches, reading-state recovery, and macOS Keychain integration

## Documentation

- [Product definition and technical implementation plan](docs/atlas-reader-product-spec.md)

## Architecture

The repository is a Cargo and pnpm workspace:

```text
apps/desktop              Tauri shell and React frontend
crates/atlas-domain       Framework-independent domain and IPC types
crates/atlas-library      Deep local-library module
crates/atlas-document-reader
                          Reader module: source authorization and reading position
crates/atlas-reading-session
                          ReadingSession interface and implementation
crates/atlas-storage      SQLite migrations and storage adapters
crates/atlas-adapters     External-provider adapters
crates/atlas-contracts    Rust contract facade
packages/contracts        Generated TypeScript contracts
```

Dependencies point inward toward `atlas-domain`. React talks to Rust through the Tauri bridge, and
callers never orchestrate parsing, translation, retries, or caching directly.

## Development

Requirements:

- Node.js 24.15
- pnpm 10.33
- Rust 1.97
- Xcode command-line tools

```bash
pnpm install
pnpm contracts:generate
pnpm validate
pnpm tauri:dev
```

`pnpm validate` runs formatting, linting, type checks, frontend tests, frontend builds, Clippy, and
Rust tests. Cloud MinerU live tests are not part of the default suite and require an explicitly
supplied test key.

## License

No open-source license has been selected yet. Public repository visibility does not grant permission
to copy, modify, or redistribute the contents.
