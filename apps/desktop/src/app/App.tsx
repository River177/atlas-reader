import { useState } from "react";
import type { DocumentSummary } from "@atlas/contracts";

import { atlasBridge, type AtlasBridge } from "../bridge";
import { LibraryView } from "../features/library/LibraryView";
import { useLibrary } from "../features/library/useLibrary";
import { ReaderScreen } from "../features/reader/ReaderScreen";
import type { PdfViewerFactory } from "../features/reader/pdf-viewer-module";

interface AppProps {
  bridge?: AtlasBridge;
  viewerFactory?: PdfViewerFactory;
}

export function App({ bridge = atlasBridge, viewerFactory }: AppProps) {
  const [activeDocument, setActiveDocument] = useState<DocumentSummary>();

  if (activeDocument) {
    return (
      <ReaderScreen
        bridge={bridge}
        document={activeDocument}
        onBack={() => setActiveDocument(undefined)}
        {...(viewerFactory ? { viewerFactory } : {})}
      />
    );
  }

  return <LibraryScreen bridge={bridge} onOpen={setActiveDocument} />;
}

interface LibraryScreenProps {
  bridge: AtlasBridge;
  onOpen(document: DocumentSummary): void;
}

function LibraryScreen({ bridge, onOpen }: LibraryScreenProps) {
  const library = useLibrary(bridge);

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <span className="brand-mark" aria-hidden="true">
            A
          </span>
          <div>
            <strong>Atlas Reader</strong>
            <span>Local research desk</span>
          </div>
        </div>

        <nav aria-label="Primary navigation">
          <button className="nav-item nav-item--active" type="button">
            <span>Library</span>
            <span className="nav-count">{library.documents.length}</span>
          </button>
          <button className="nav-item" disabled type="button">
            Reading queue
          </button>
          <button className="nav-item" disabled type="button">
            Settings
          </button>
        </nav>

        <div className="runtime-note">
          <span className="status-dot" aria-hidden="true" />
          <div>
            <strong>Local core connected</strong>
            <span>Schema v1 · Offline safe</span>
          </div>
        </div>
      </aside>

      <main className="workspace">
        <header className="workspace-header">
          <div>
            <span className="eyebrow">Research library</span>
            <h1>Read difficult papers without losing the thread.</h1>
          </div>
          <div className="header-actions">
            <button
              className="secondary-action"
              disabled={library.operation !== undefined}
              onClick={() => void library.refresh()}
              type="button"
            >
              Refresh sources
            </button>
            <button
              className="primary-action"
              disabled={library.operation !== undefined}
              onClick={() => void library.importFromPicker()}
              type="button"
            >
              {library.operation === "import" ? "Importing…" : "Import PDF"}
            </button>
          </div>
        </header>

        <div className="library-toolbar">
          <form
            className="search-form"
            onSubmit={(event) => {
              event.preventDefault();
              void library.search();
            }}
          >
            <label htmlFor="library-search">Search library</label>
            <div>
              <input
                disabled={library.operation !== undefined}
                id="library-search"
                onChange={(event) => library.setSearchText(event.currentTarget.value)}
                placeholder="Title or author"
                type="search"
                value={library.searchText}
              />
              <button
                className="secondary-action"
                disabled={library.operation !== undefined}
                type="submit"
              >
                Search
              </button>
            </div>
          </form>
          <span className="library-count">
            {library.documents.length} paper{library.documents.length === 1 ? "" : "s"}
          </span>
        </div>

        {library.notice ? (
          <div
            className={`notice notice--${library.notice.kind}`}
            role={library.notice.kind === "error" ? "alert" : "status"}
          >
            {library.notice.message}
          </div>
        ) : null}

        <section className="workspace-body" aria-label="Paper library">
          <LibraryView
            busyDocumentId={library.busyDocumentId}
            documents={library.documents}
            error={library.error}
            loading={library.loading}
            onImport={() => void library.importFromPicker()}
            onOpen={onOpen}
            onRelocate={(document) => void library.relocate(document)}
            onRemove={(document) => void library.remove(document)}
          />
        </section>

        <footer className="foundation-grid" aria-label="Foundation status">
          <div>
            <span>01</span>
            <strong>Validated imports</strong>
            <p>PDF type, size, page count, metadata, and SHA-256 are checked locally.</p>
          </div>
          <div>
            <span>02</span>
            <strong>Referenced files</strong>
            <p>Atlas stores the original path and never deletes the source PDF.</p>
          </div>
          <div>
            <span>03</span>
            <strong>Duplicate safe</strong>
            <p>Identical content reuses one library record and refreshes its source path.</p>
          </div>
        </footer>
      </main>

      {library.dropActive ? (
        <div className="drop-overlay" role="status">
          <div>
            <span>Drop to import</span>
            <strong>PDF papers only</strong>
          </div>
        </div>
      ) : null}
    </div>
  );
}
