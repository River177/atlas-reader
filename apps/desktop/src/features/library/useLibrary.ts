import { useCallback, useEffect, useRef, useState } from "react";
import type { DocumentId, DocumentSummary } from "@atlas/contracts";

import type { AtlasBridge, PdfDropEvent } from "../../bridge";
import { errorMessage } from "../../bridge/error-message";

export interface LibraryNotice {
  kind: "success" | "warning" | "error";
  message: string;
}

type LibraryOperation = "import" | "refresh" | "relocate" | "remove";

export function useLibrary(bridge: AtlasBridge) {
  const [documents, setDocuments] = useState<DocumentSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>();
  const [notice, setNotice] = useState<LibraryNotice>();
  const [searchText, setSearchText] = useState("");
  const [operation, setOperation] = useState<LibraryOperation>();
  const [busyDocumentId, setBusyDocumentId] = useState<DocumentId>();
  const [dropActive, setDropActive] = useState(false);
  const operationLock = useRef(false);

  const queryDocuments = useCallback(
    async (text = searchText) => {
      const normalizedText = text.trim();
      const page = await bridge.queryLibrary({
        sort: "recent",
        limit: 100,
        ...(normalizedText ? { text: normalizedText } : {}),
      });
      setDocuments(page.items);
      setError(undefined);
    },
    [bridge, searchText],
  );

  useEffect(() => {
    let active = true;
    void (async () => {
      try {
        const refresh = await bridge.refreshLibrarySources();
        const page = await bridge.queryLibrary({
          sort: "recent",
          limit: 100,
        });
        if (!active) {
          return;
        }
        setDocuments(page.items);
        if (refresh.updated.length > 0) {
          setNotice({
            kind: "warning",
            message: sourceRefreshMessage(refresh.updated),
          });
        }
      } catch (reason) {
        if (active) {
          setError(errorMessage(reason));
        }
      } finally {
        if (active) {
          setLoading(false);
        }
      }
    })();

    return () => {
      active = false;
    };
  }, [bridge]);

  const importPaths = useCallback(
    async (paths: string[]) => {
      const uniquePaths = [...new Set(paths)].filter(Boolean);
      if (uniquePaths.length === 0 || operationLock.current) {
        return;
      }

      operationLock.current = true;
      setOperation("import");
      setNotice(undefined);
      const failures: string[] = [];
      let imported = 0;
      let duplicates = 0;

      try {
        for (const path of uniquePaths) {
          try {
            const result = await bridge.importPdf(path);
            if (result.duplicate) {
              duplicates += 1;
            } else {
              imported += 1;
            }
          } catch (reason) {
            failures.push(errorMessage(reason));
          }
        }
        if (imported > 0 || duplicates > 0) {
          await queryDocuments();
        }
        setNotice(importNotice(imported, duplicates, failures));
      } finally {
        operationLock.current = false;
        setOperation(undefined);
      }
    },
    [bridge, queryDocuments],
  );

  useEffect(() => {
    let active = true;
    let unsubscribe: (() => void) | undefined;

    void bridge
      .subscribePdfDrops((event: PdfDropEvent) => {
        if (event.type === "enter") {
          setDropActive(true);
        } else if (event.type === "leave") {
          setDropActive(false);
        } else {
          setDropActive(false);
          void importPaths(event.paths);
        }
      })
      .then((listener) => {
        if (active) {
          unsubscribe = listener;
        } else {
          listener();
        }
      })
      .catch((reason: unknown) => {
        if (active) {
          setNotice({
            kind: "error",
            message: errorMessage(reason),
          });
        }
      });

    return () => {
      active = false;
      unsubscribe?.();
    };
  }, [bridge, importPaths]);

  const importFromPicker = useCallback(async () => {
    try {
      await importPaths(await bridge.pickPdfPaths(true));
    } catch (reason) {
      setNotice({ kind: "error", message: errorMessage(reason) });
    }
  }, [bridge, importPaths]);

  const search = useCallback(async () => {
    if (operationLock.current) {
      return;
    }
    operationLock.current = true;
    setOperation("refresh");
    try {
      await queryDocuments(searchText);
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      operationLock.current = false;
      setOperation(undefined);
    }
  }, [queryDocuments, searchText]);

  const refresh = useCallback(async () => {
    if (operationLock.current) {
      return;
    }
    operationLock.current = true;
    setOperation("refresh");
    try {
      const result = await bridge.refreshLibrarySources();
      await queryDocuments();
      setNotice(
        result.updated.length === 0
          ? { kind: "success", message: "All local PDF sources are available." }
          : { kind: "warning", message: sourceRefreshMessage(result.updated) },
      );
    } catch (reason) {
      setNotice({ kind: "error", message: errorMessage(reason) });
    } finally {
      operationLock.current = false;
      setOperation(undefined);
    }
  }, [bridge, queryDocuments]);

  const relocate = useCallback(
    async (document: DocumentSummary) => {
      if (operationLock.current) {
        return;
      }
      try {
        const [newPath] = await bridge.pickPdfPaths(false);
        if (!newPath) {
          return;
        }
        operationLock.current = true;
        setOperation("relocate");
        setBusyDocumentId(document.id);
        await bridge.relocateDocument(document.id, newPath);
        await queryDocuments();
        setNotice({
          kind: "success",
          message: `Relocated “${document.title}”.`,
        });
      } catch (reason) {
        setNotice({ kind: "error", message: errorMessage(reason) });
      } finally {
        operationLock.current = false;
        setOperation(undefined);
        setBusyDocumentId(undefined);
      }
    },
    [bridge, queryDocuments],
  );

  const remove = useCallback(
    async (document: DocumentSummary) => {
      if (operationLock.current || !(await bridge.confirmDocumentRemoval(document.title))) {
        return;
      }
      operationLock.current = true;
      setOperation("remove");
      setBusyDocumentId(document.id);
      try {
        await bridge.removeDocument(document.id);
        await queryDocuments();
        setNotice({
          kind: "success",
          message: `Removed “${document.title}” from Atlas Reader. The PDF was kept.`,
        });
      } catch (reason) {
        setNotice({ kind: "error", message: errorMessage(reason) });
      } finally {
        operationLock.current = false;
        setOperation(undefined);
        setBusyDocumentId(undefined);
      }
    },
    [bridge, queryDocuments],
  );

  return {
    documents,
    loading,
    error,
    notice,
    searchText,
    operation,
    busyDocumentId,
    dropActive,
    setSearchText,
    importFromPicker,
    search,
    refresh,
    relocate,
    remove,
  };
}

function importNotice(imported: number, duplicates: number, failures: string[]): LibraryNotice {
  const successes = [
    imported > 0 ? `${imported} imported` : "",
    duplicates > 0 ? `${duplicates} already in the library` : "",
  ].filter(Boolean);
  if (failures.length === 0) {
    return {
      kind: "success",
      message: successes.join(" · ") || "No PDFs were selected.",
    };
  }
  return {
    kind: successes.length > 0 ? "warning" : "error",
    message: [...successes, `${failures.length} failed: ${failures[0]}`].join(" · "),
  };
}

function sourceRefreshMessage(documents: DocumentSummary[]): string {
  const unavailable = documents.filter((document) => document.sourceState !== "available");
  if (unavailable.length === 0) {
    return `${documents.length} local PDF source${documents.length === 1 ? "" : "s"} restored.`;
  }
  return `${unavailable.length} local PDF source${unavailable.length === 1 ? "" : "s"} need attention.`;
}
