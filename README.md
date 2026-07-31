# Atlas Reader

Atlas Reader is a macOS bilingual academic PDF reader for Chinese researchers reading English
papers.

The product focuses on chapter-based English–Chinese close reading, preserving document structure,
formulas, citations, and page relationships. Selected translated text can ground a persistent
document-scoped Reading Assistant without changing the translation. Cloud MinerU provides document
parsing with a user-supplied API key, while translation and chat use a user-configured
OpenAI-compatible endpoint.

## Status

Atlas Reader's local foundation, parsing loop, and chapter translation loop are complete. The
repository contains a runnable Tauri 2 desktop shell, a React and TypeScript frontend, Rust domain
modules, SQLite migrations, generated TypeScript contracts, and end-to-end slice tests.

Completed vertical slices:

1. **Local library import** — import PDFs from the native picker or drag and drop, search the local
   library, detect duplicates, refresh source status, relocate moved files, and remove records
   without deleting the original PDF.
2. **Original PDF reading** — open an imported paper in an embedded PDF.js viewer with page
   navigation, zoom, in-document search, and a reading position that survives closing the reader and
   restarting the application. The viewer reaches the PDF through a capability-token URL served by a
   dedicated `atlas-reader://` protocol, so absolute file paths never reach the frontend and a token
   stops working as soon as the source file changes or the reader closes.

3. **Provider settings** — configure the Cloud MinerU endpoint and any OpenAI-compatible translation
   endpoint, store each API key in the macOS Keychain, and test a connection with a result that
   distinguishes DNS, TLS, authorization, rate limiting, protocol mismatch, and timeout. A key is
   never readable back through any interface, plain HTTP is accepted only for loopback endpoints,
   and the settings screen states what a paper upload would send, where, and under whose credential
   before automatic cloud parsing can be switched on.
4. **Parsing loop** — automatically parse uncached papers with Cloud MinerU when enabled, persist
   the remote batch before uploading, resume interrupted work without allocating a duplicate batch,
   and fall back to the local PDF text layer on a definite remote failure. Results are normalized
   into a provider-neutral chapter/block schema, published transactionally to SQLite and a
   content-addressed artifact cache, and rendered as a chapter outline with structured text,
   formulas, tables, figures, captions, and citations. Archive extraction rejects traversal, links,
   oversized entries, and zip bombs; ambiguous remote outcomes require an explicit recovery choice.
5. **Translation loop** — translate the focused chapter through an OpenAI-compatible streaming
   endpoint, preserve formulas, citations, line breaks, assets, and table cell structure, validate
   each block before publishing it, repair only failed blocks once, and cache results by source,
   endpoint, model, prompt, mode, locale, and applicable preferences. The Translation Module owns
   retries, partial commits, interruption recovery, foreground preemption, and one-chapter prefetch;
   React only sends chapter intent, while polling uses a read-only projection that cannot preempt
   work.

Provider credential lookup failures are non-fatal to local use: cached translations, cached parse
artifacts, and local PDF text remain readable. Persisted cloud parse work resumes on the next
`ensure` after the provider becomes available again. Resume requires the original endpoint
fingerprint; changing the endpoint turns old work into an explicit re-upload choice rather than
sending the current credential to the previous host.

The next vertical slice is a document-scoped Reading Assistant: select translated text, attach its
aligned source context to the left chat panel, stream an explanation, persist the conversation
locally, and navigate validated citations back to the paper. Chat never modifies translations.

### Cloud MinerU protocol verification

The Cloud MinerU risk spike ran against the live API on 2026-07-30, so the parsing slice is designed
against measured behaviour rather than documentation. Ten arXiv papers finished within 120 seconds
each, at a P75 of 25.8 seconds and a worst case of 68.8 seconds for a 75-page paper. The upload,
polling, download, result layout, coordinate system, and error envelopes are recorded in section 18
of the product specification.

`crates/atlas-adapters/tests/live_mineru.rs` pins the connection probe against the real endpoint. It
is skipped unless `ATLAS_LIVE_MINERU=1` is set, so ordinary runs and CI stay offline and free:

```sh
ATLAS_LIVE_MINERU=1 cargo test -p atlas-adapters --test live_mineru
```

The test reads its credential from the same Keychain entry the application writes, so no key is
needed in the repository. Save a Cloud MinerU key in Atlas settings first, or export
`ATLAS_CLOUD_MINERU` to skip the keychain entirely — see [Credentials](#credentials).

### Translation protocol verification

The OpenAI-compatible streaming spike ran against a live endpoint on 2026-07-30. Twelve real paper
blocks carrying eighteen protection markers were translated three times each across three models,
with and without `response_format`. All eighteen runs preserved block count, block order, and every
protection marker verbatim. Hostile text embedded in the source was translated rather than obeyed.
Section 19 of the product specification records the measured baseline and the three behaviours the
stream parser must tolerate: Markdown code fences, a JSON array in place of JSON Lines, and a final
record with no trailing newline.

The decisive finding is that the output field names must be pinned with a literal example. Asking
only for "one JSON object per block" produced three mutually incompatible shapes across models.

`crates/atlas-adapters/tests/live_translation.rs` pins both the connection probe and one synthetic
block through the complete streaming/planning/validation protocol. It is skipped unless
`ATLAS_LIVE_TRANSLATION=1` is set:

```sh
ATLAS_LIVE_TRANSLATION=1 cargo test -p atlas-adapters --test live_translation
```

Set `ATLAS_LIVE_TRANSLATION_URL` to choose the endpoint; it defaults to `http://127.0.0.1:4141/v1`.
The test resolves the active profile's Keychain account from the local Atlas database. Save a
translation key in Atlas settings first, or export `ATLAS_OPENAI_COMPATIBLE` — see
[Credentials](#credentials). `ATLAS_APP_DATABASE_PATH` can point the test at a non-default database.

### Credentials

Credentials live in the macOS keychain under service `com.atlasreader.providers`. **Never put an API
key in this repository.** It is public, and anything committed to git history is leaked permanently
even if a later commit removes it.

macOS ties keychain access control to the calling binary's code signature. Development builds are
ad-hoc signed and the linker embeds the cargo build hash in the signing identity, so every rebuild
looks like a brand new application to the keychain and triggers an authorization prompt. Choosing
"Always Allow" does not help, because the next build replaces the binary that was authorized.

Debug builds therefore read an environment variable before touching the keychain:

| Provider          | Keychain account                     | Environment variable      |
| ----------------- | ------------------------------------ | ------------------------- |
| Cloud MinerU      | `atlas.cloud_mineru__<version>`      | `ATLAS_CLOUD_MINERU`      |
| Translation model | `atlas.openai_compatible__<version>` | `ATLAS_OPENAI_COMPATIBLE` |

The profile stores the current versioned account. Replacing a key writes a fresh account before
atomically switching the profile, so a crash can leave only an unreachable orphan; it cannot pair a
new credential with the previous endpoint. Legacy unversioned accounts remain readable.

Export them in your shell and development stops prompting:

```sh
export ATLAS_CLOUD_MINERU='...'
export ATLAS_OPENAI_COMPATIBLE='...'
```

The override shadows reads only; saving a key in settings still writes to the keychain. Blank values
are treated as unset and fall through to the keychain. **Release builds ignore these variables
entirely**, so a shipped Atlas reads credentials only from the keychain — and because a signed app
keeps a stable identity across launches and upgrades, users are prompted at most once.

## MVP direction

- Lightweight local paper library
- Original PDF reading
- Automatic Cloud MinerU parsing with a user-supplied API key
- Chapter-based bilingual reading
- Formula, citation, and block-structure preservation
- Selection-grounded Reading Assistant with persistent document conversations
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
crates/atlas-provider-settings
                          Provider endpoints, credentials, and connection tests
crates/atlas-parse         Parse orchestration, canonical normalization, and safe artifacts
crates/atlas-translation   Chapter planning, validation, caching, recovery, and prefetch
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
Rust tests. Cloud MinerU and translation live tests are not part of the default suite and require
explicitly supplied credentials.

## License

No open-source license has been selected yet. Public repository visibility does not grant permission
to copy, modify, or redistribute the contents.
