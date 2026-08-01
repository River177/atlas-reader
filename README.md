# Atlas Reader

Atlas Reader is a local web application for Chinese researchers reading English academic PDFs.

The product focuses on chapter-based English–Chinese close reading, preserving document structure,
formulas, citations, and page relationships. Selected translated text can ground a persistent
document-scoped Reading Assistant without changing the translation. Cloud MinerU provides document
parsing with a user-supplied API key, while translation and chat use a user-configured
OpenAI-compatible endpoint.

## Status

Atlas Reader's library, reading, parsing, translation, and Reading Assistant loops are complete. A
loopback-only Rust server hosts the React web application and owns SQLite, Keychain access, managed
PDF copies, provider calls, recovery, and protected reader media.

Completed vertical slices:

1. **Local library import** — import PDFs from the browser picker or drag and drop, stream them into
   Atlas-managed local storage, validate metadata and limits, detect duplicates, search, replace
   missing legacy sources, and remove managed copies.
2. **Original PDF reading** — open an imported paper in a PDF.js viewer with page navigation, zoom,
   in-document search, and a reading position that survives closing the reader and restarting the
   server. PDF bytes use an opaque capability URL with HTTP Range support, so filesystem paths never
   reach the browser.

3. **Provider settings** — configure the Cloud MinerU endpoint and any OpenAI-compatible translation
   endpoint, store each API key in the macOS Keychain, and test a connection with a result that
   distinguishes DNS, TLS, authorization, rate limiting, protocol mismatch, and timeout. A key is
   never readable back through any interface, plain HTTP is accepted only for loopback endpoints,
   and the settings screen states what a paper upload would send, where, and under whose credential
   before automatic cloud parsing can be switched on.
4. **Parsing loop** — automatically parse uncached papers with Cloud MinerU when enabled, persist
   the remote batch before uploading, resume interrupted work without allocating a duplicate batch,
   and surface remote failures without publishing a low-quality local structure. Results are
   normalized into a provider-neutral chapter/block schema, published transactionally to SQLite and
   a content-addressed artifact cache, and rendered as a chapter outline with structured text,
   formulas, tables, figures, captions, and citations. Archive extraction rejects traversal, links,
   oversized entries, and zip bombs; ambiguous remote outcomes require an explicit recovery choice.
5. **Translation loop** — translate the focused chapter through an OpenAI-compatible streaming
   endpoint, preserve formulas, citations, line breaks, assets, and table cell structure, validate
   each block before publishing it, repair only failed blocks once, and cache results by source,
   endpoint, model, prompt, mode, locale, and applicable preferences. The Translation Module owns
   retries, partial commits, interruption recovery, foreground preemption, and one-chapter prefetch;
   React only sends chapter intent, while polling uses a read-only projection that cannot preempt
   work.

Provider credential lookup failures are non-fatal to local use: cached translations, cached Cloud
parse artifacts, and the original PDF remain readable. Persisted cloud parse work resumes on the
next `ensure` after the provider becomes available again. Resume requires the original endpoint
fingerprint; changing the endpoint turns old work into an explicit re-upload choice rather than
sending the current credential to the previous host.

The Reading Assistant is document-scoped: selected translated text attaches validated aligned source
context to the left chat, responses stream and persist locally, and validated citations navigate
back to the paper. Chat never modifies translations.

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

macOS ties Keychain access to the calling binary's code signature. `pnpm web:build` signs the local
Rust server with an installed Apple Development identity plus `com.atlasreader.desktop`. Select
"Always Allow" once for each existing Atlas credential; that choice survives local rebuilds.

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
entirely**, so a shipped Atlas reads credentials only from the keychain. Startup recovery does not
touch provider credentials when no jobs need recovery.

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
apps/web                  React browser frontend
apps/web-server           Loopback-only Axum server and HTTP Adapter
crates/atlas-domain       Framework-independent domain and transport contracts
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

Dependencies point inward toward `atlas-domain`. React uses one HTTP `AtlasBridge` Adapter; callers
never orchestrate parsing, Translation, Reading Assistant retries, recovery, or caching directly.

## Development

Requirements:

- Node.js 24.15
- pnpm 10.33
- Rust 1.97
- Xcode command-line tools
- An Apple Development signing identity for prompt-free local Release builds

```bash
pnpm install
pnpm contracts:generate
pnpm validate
pnpm web:start
```

`pnpm web:start` builds the frontend and Rust server, packages static assets beside the binary,
applies a stable local signature on macOS, starts on an ephemeral loopback port, and opens the
one-time launch URL in the default browser. `pnpm web:run` starts an already-built server.

`pnpm validate` runs formatting, linting, type checks, frontend tests, frontend builds, Clippy, and
Rust tests. Cloud MinerU and translation live tests are not part of the default suite and require
explicitly supplied credentials.

## License

No open-source license has been selected yet. Public repository visibility does not grant permission
to copy, modify, or redistribute the contents.
