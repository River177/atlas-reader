import { useEffect, useState } from "react";
import type { DocumentSummary } from "@atlas/contracts";

import { atlasBridge, type AtlasBridge } from "../bridge";
import { LibraryView } from "../features/library/LibraryView";
import { useLibrary } from "../features/library/useLibrary";
import { ReaderScreen } from "../features/reader/ReaderScreen";
import type { PdfViewerFactory } from "../features/reader/pdf-viewer-module";
import { SettingsScreen } from "../features/settings/SettingsScreen";
import { WorkspaceSidebar, type WorkspaceView } from "./WorkspaceSidebar";

interface AppProps {
  bridge?: AtlasBridge;
  viewerFactory?: PdfViewerFactory;
}

export function App({ bridge = atlasBridge, viewerFactory }: AppProps) {
  const [activeDocument, setActiveDocument] = useState<DocumentSummary>();
  const [view, setView] = useState<WorkspaceView>("library");

  useEffect(() => {
    const restore = (event: PageTransitionEvent) => {
      if (event.persisted) {
        window.location.reload();
      }
    };
    window.addEventListener("pageshow", restore);
    return () => window.removeEventListener("pageshow", restore);
  }, []);

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

  if (view === "settings") {
    return (
      <div className="app-shell">
        <WorkspaceSidebar activeView="settings" onNavigate={setView} />
        <main className="workspace">
          <SettingsScreen bridge={bridge} />
        </main>
      </div>
    );
  }

  return <LibraryScreen bridge={bridge} onNavigate={setView} onOpen={setActiveDocument} />;
}

interface LibraryScreenProps {
  bridge: AtlasBridge;
  onNavigate(view: WorkspaceView): void;
  onOpen(document: DocumentSummary): void;
}

function LibraryScreen({ bridge, onNavigate, onOpen }: LibraryScreenProps) {
  const library = useLibrary(bridge);

  return (
    <div className="app-shell">
      <WorkspaceSidebar
        activeView="library"
        libraryCount={library.documents.length}
        onNavigate={onNavigate}
      />

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
            <strong>Managed copies</strong>
            <p>Browser uploads stay on this machine in Atlas-managed storage.</p>
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
