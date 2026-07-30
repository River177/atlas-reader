import { useEffect, useState } from "react";
import type { DocumentSummary } from "@atlas/contracts";

import { atlasBridge, type AtlasBridge } from "../bridge";
import { LibraryView } from "../features/library/LibraryView";

interface AppProps {
  bridge?: AtlasBridge;
}

export function App({ bridge = atlasBridge }: AppProps) {
  const [documents, setDocuments] = useState<DocumentSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>();

  useEffect(() => {
    let active = true;

    void bridge
      .queryLibrary({
        sort: "recent",
        limit: 30,
      })
      .then((page) => {
        if (active) {
          setDocuments(page.items);
          setLoading(false);
        }
      })
      .catch((reason: unknown) => {
        if (active) {
          setError(reason instanceof Error ? reason.message : "Unknown library error");
          setLoading(false);
        }
      });

    return () => {
      active = false;
    };
  }, [bridge]);

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
            <span className="nav-count">{documents.length}</span>
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
          <div className="environment-chip">
            <span className="status-dot" aria-hidden="true" />
            Development foundation
          </div>
        </header>

        <section className="workspace-body" aria-label="Paper library">
          <LibraryView documents={documents} loading={loading} error={error} />
        </section>

        <footer className="foundation-grid" aria-label="Foundation status">
          <div>
            <span>01</span>
            <strong>Local library</strong>
            <p>SQLite migrations and a paginated Library interface are active.</p>
          </div>
          <div>
            <span>02</span>
            <strong>ReadingSession</strong>
            <p>Commands are revisioned, idempotent, and isolated behind one seam.</p>
          </div>
          <div>
            <span>03</span>
            <strong>External providers</strong>
            <p>MinerU and translation remain unconfigured until user credentials exist.</p>
          </div>
        </footer>
      </main>
    </div>
  );
}
