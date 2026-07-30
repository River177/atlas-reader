import type { DocumentSummary } from "@atlas/contracts";

interface LibraryViewProps {
  documents: DocumentSummary[];
  loading: boolean;
  error: string | undefined;
}

export function LibraryView({ documents, loading, error }: LibraryViewProps) {
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
          The local database, ReadingSession interface, and desktop bridge are connected. PDF import
          is the next vertical slice.
        </p>
      </div>
    );
  }

  return (
    <div className="paper-list">
      {documents.map((document) => (
        <article className="paper-row" key={document.id}>
          <div>
            <span className="paper-meta">
              {document.pageCount ? `${document.pageCount} pages` : "Page count pending"}
            </span>
            <h2>{document.title}</h2>
            <p>{document.authors.join(", ") || "Authors pending"}</p>
          </div>
          <span
            className={
              document.sourceAvailable ? "file-status" : "file-status file-status--missing"
            }
          >
            {document.sourceAvailable ? "Local source" : "Source missing"}
          </span>
        </article>
      ))}
    </div>
  );
}
