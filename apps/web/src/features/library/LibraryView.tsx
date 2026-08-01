import type { DocumentSummary } from "@atlas/contracts";

interface LibraryViewProps {
  documents: DocumentSummary[];
  loading: boolean;
  error: string | undefined;
  busyDocumentId: string | undefined;
  onImport: () => void;
  onOpen: (document: DocumentSummary) => void;
  onRelocate: (document: DocumentSummary) => void;
  onRemove: (document: DocumentSummary) => void;
}

const sourceLabels = {
  available: "Managed source",
  missing: "Source missing",
  changed: "Source changed",
  unreadable: "Source unreadable",
} as const;

export function LibraryView({
  documents,
  loading,
  error,
  busyDocumentId,
  onImport,
  onOpen,
  onRelocate,
  onRemove,
}: LibraryViewProps) {
  if (loading) {
    return (
      <div className="library-state" role="status">
        <span className="state-kicker">Local library</span>
        <h2>Opening your workspace</h2>
        <p>Atlas is checking the local database and restoring reading state.</p>
      </div>
    );
  }

  if (error) {
    return (
      <div className="library-state library-state--error" role="alert">
        <span className="state-kicker">Local library unavailable</span>
        <h2>Atlas could not open the library</h2>
        <p>{error}</p>
      </div>
    );
  }

  if (documents.length === 0) {
    return (
      <div className="library-state">
        <div className="empty-mark" aria-hidden="true">
          AR
        </div>
        <span className="state-kicker">Foundation ready</span>
        <h2>Your research library is empty</h2>
        <p>
          Import a paper to add a managed local copy. Atlas validates the PDF, extracts basic
          metadata, and detects duplicates before saving the record.
        </p>
        <button className="primary-action empty-action" onClick={onImport} type="button">
          Choose PDF files
        </button>
        <span className="drop-hint">or drop PDFs anywhere in this window</span>
      </div>
    );
  }

  return (
    <div className="paper-list">
      {documents.map((document) => (
        <article className="paper-row" key={document.id}>
          <div>
            <span className="paper-meta">
              {document.pageCount ? `${document.pageCount} pages` : "Page count pending"} ·{" "}
              {document.fileName}
            </span>
            <h2>
              <button
                className="paper-title-button"
                disabled={document.sourceState !== "available"}
                onClick={() => onOpen(document)}
                type="button"
              >
                {document.title}
              </button>
            </h2>
            <p>{document.authors.join(", ") || "Authors pending"}</p>
          </div>
          <div className="paper-actions">
            <span
              className={
                document.sourceState === "available"
                  ? "file-status"
                  : "file-status file-status--missing"
              }
            >
              {sourceLabels[document.sourceState]}
            </span>
            <button
              className="text-action"
              disabled={busyDocumentId === document.id || document.sourceState !== "available"}
              onClick={() => onOpen(document)}
              type="button"
            >
              Open
            </button>
            <button
              className="text-action"
              disabled={busyDocumentId === document.id}
              onClick={() => onRelocate(document)}
              type="button"
            >
              Locate
            </button>
            <button
              className="text-action text-action--danger"
              disabled={busyDocumentId === document.id}
              onClick={() => onRemove(document)}
              type="button"
            >
              Remove
            </button>
          </div>
        </article>
      ))}
    </div>
  );
}
