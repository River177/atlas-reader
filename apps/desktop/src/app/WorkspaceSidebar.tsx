export type WorkspaceView = "library" | "settings";

interface WorkspaceSidebarProps {
  activeView: WorkspaceView;
  libraryCount?: number;
  onNavigate(view: WorkspaceView): void;
}

export function WorkspaceSidebar({ activeView, libraryCount, onNavigate }: WorkspaceSidebarProps) {
  return (
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
        <button
          aria-current={activeView === "library" ? "page" : undefined}
          className={activeView === "library" ? "nav-item nav-item--active" : "nav-item"}
          onClick={() => onNavigate("library")}
          type="button"
        >
          <span>Library</span>
          {libraryCount === undefined ? null : <span className="nav-count">{libraryCount}</span>}
        </button>
        <button className="nav-item" disabled type="button">
          Reading queue
        </button>
        <button
          aria-current={activeView === "settings" ? "page" : undefined}
          className={activeView === "settings" ? "nav-item nav-item--active" : "nav-item"}
          onClick={() => onNavigate("settings")}
          type="button"
        >
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
  );
}
