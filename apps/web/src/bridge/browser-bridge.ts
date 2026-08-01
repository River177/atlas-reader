import type {
  CommandReceipt,
  ConnectionTestResult,
  DocumentSummary,
  ImportPdfResult,
  LibraryPage,
  OpenedReaderDocument,
  OpenSessionResult,
  ParseSnapshot,
  ParsedDocumentView,
  PublicProviderSettings,
  ReadingPosition,
  RefreshSourcesResult,
  SessionSnapshot,
} from "@atlas/contracts";

import type { AtlasBridge, DispatchReadingCommandInput } from "./atlas-bridge";

interface ServerBootstrapResult {
  accessToken: string;
  csrfToken: string;
  resourceToken: string;
}

interface BootstrapResult extends ServerBootstrapResult {
  clientId: string;
}

const sessionStorageKey = "atlas.web.session";
const browsingContextId = window.crypto.randomUUID();
let bootstrapPromise: Promise<BootstrapResult> | undefined;
let activeTokens: BootstrapResult | undefined;
const activeReaderTokens = new Set<string>();
const activeSessionIds = new Set<string>();
let heartbeatTimer: number | undefined;

async function bootstrap(): Promise<BootstrapResult> {
  if (!bootstrapPromise) {
    bootstrapPromise = (async () => {
      const launchToken = new URLSearchParams(window.location.hash.slice(1)).get("launch");
      const stored = window.sessionStorage.getItem(sessionStorageKey);
      const previous = stored ? (JSON.parse(stored) as BootstrapResult) : undefined;
      if (!launchToken && !previous) {
        throw new Error("Open Atlas Reader from the local server launch URL");
      }
      const response = await fetch(
        launchToken ? "/api/bootstrap/exchange" : "/api/bootstrap/session",
        launchToken
          ? {
              method: "POST",
              credentials: "same-origin",
              headers: { "content-type": "application/json" },
              body: JSON.stringify({ launchToken }),
            }
          : {
              headers: {
                authorization: `Bearer ${previous?.accessToken ?? ""}`,
                "x-atlas-client": browsingContextId,
              },
            },
      );
      if (!response.ok) {
        throw await responseError(response);
      }
      if (launchToken) {
        window.history.replaceState(
          null,
          "",
          `${window.location.pathname}${window.location.search}`,
        );
      }
      const serverTokens = (await response.json()) as ServerBootstrapResult;
      const tokens: BootstrapResult = {
        ...serverTokens,
        clientId: browsingContextId,
      };
      window.sessionStorage.setItem(sessionStorageKey, JSON.stringify(tokens));
      activeTokens = tokens;
      startLeaseMaintenance(tokens);
      return tokens;
    })();
  }
  return bootstrapPromise;
}

function authorizedHeaders(tokens: BootstrapResult): Headers {
  return new Headers({
    authorization: `Bearer ${tokens.accessToken}`,
    "content-type": "application/json",
    "x-atlas-csrf": tokens.csrfToken,
    "x-atlas-client": tokens.clientId,
  });
}

function leasePayload() {
  return {
    readerSourceTokens: [...activeReaderTokens],
    sessionIds: [...activeSessionIds],
  };
}

function startLeaseMaintenance(tokens: BootstrapResult) {
  if (heartbeatTimer === undefined) {
    heartbeatTimer = window.setInterval(() => {
      if (activeReaderTokens.size === 0 && activeSessionIds.size === 0) {
        return;
      }
      void fetch("/api/heartbeat", {
        method: "POST",
        headers: authorizedHeaders(tokens),
        body: JSON.stringify(leasePayload()),
      });
    }, 30_000);
  }
}

async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
  const tokens = await bootstrap();
  const headers = new Headers(init.headers);
  headers.set("authorization", `Bearer ${tokens.accessToken}`);
  headers.set("x-atlas-client", tokens.clientId);
  if (init.body !== undefined && !(init.body instanceof FormData) && !headers.has("content-type")) {
    headers.set("content-type", "application/json");
  }
  const method = init.method?.toUpperCase() ?? "GET";
  if (method !== "GET" && method !== "HEAD") {
    headers.set("x-atlas-csrf", tokens.csrfToken);
  }
  const response = await fetch(path, {
    ...init,
    method,
    headers,
    credentials: "same-origin",
  });
  if (!response.ok) {
    throw await responseError(response);
  }
  if (response.status === 204 || response.headers.get("content-length") === "0") {
    return undefined as T;
  }
  const text = await response.text();
  return (text ? JSON.parse(text) : undefined) as T;
}

async function responseError(response: Response): Promise<unknown> {
  try {
    return (await response.json()) as unknown;
  } catch {
    return new Error(`Atlas web request failed (${response.status})`);
  }
}

function keepaliveCommand(path: string, method: "POST" | "DELETE", body?: string): Promise<void> {
  const tokens = activeTokens;
  if (!tokens) {
    return request<void>(path, {
      method,
      ...(body === undefined ? {} : { body }),
      keepalive: true,
    });
  }
  return fetch(path, {
    method,
    headers: authorizedHeaders(tokens),
    ...(body === undefined ? {} : { body }),
    keepalive: true,
  }).then(async (response) => {
    if (!response.ok) {
      throw await responseError(response);
    }
  });
}

function chooseFiles(multiple: boolean): Promise<File[]> {
  return new Promise((resolve) => {
    const input = window.document.createElement("input");
    input.type = "file";
    input.accept = "application/pdf,.pdf";
    input.multiple = multiple;
    input.addEventListener(
      "change",
      () => {
        resolve(Array.from(input.files ?? []));
        input.remove();
      },
      { once: true },
    );
    input.addEventListener(
      "cancel",
      () => {
        resolve([]);
        input.remove();
      },
      { once: true },
    );
    input.click();
  });
}

function upload(file: File): FormData {
  const body = new FormData();
  body.append("file", file, file.name);
  return body;
}

function providerPath(provider: "mineru" | "translation"): string {
  return encodeURIComponent(provider);
}

export const browserBridge: AtlasBridge = {
  pickPdfFiles: chooseFiles,
  async subscribePdfDrops(listener) {
    let depth = 0;
    const enter = (event: DragEvent) => {
      event.preventDefault();
      depth += 1;
      if (event.dataTransfer?.types.includes("Files")) {
        listener({ type: "enter" });
      }
    };
    const over = (event: DragEvent) => event.preventDefault();
    const leave = (event: DragEvent) => {
      event.preventDefault();
      depth = Math.max(0, depth - 1);
      if (depth === 0) {
        listener({ type: "leave" });
      }
    };
    const drop = (event: DragEvent) => {
      event.preventDefault();
      depth = 0;
      const files = Array.from(event.dataTransfer?.files ?? []).filter(
        (file) => file.type === "application/pdf" || file.name.toLowerCase().endsWith(".pdf"),
      );
      listener(files.length > 0 ? { type: "drop", files } : { type: "leave" });
    };
    window.addEventListener("dragenter", enter);
    window.addEventListener("dragover", over);
    window.addEventListener("dragleave", leave);
    window.addEventListener("drop", drop);
    return () => {
      window.removeEventListener("dragenter", enter);
      window.removeEventListener("dragover", over);
      window.removeEventListener("dragleave", leave);
      window.removeEventListener("drop", drop);
    };
  },
  async confirmDocumentRemoval(title) {
    return window.confirm(
      `Remove “${title}” from Atlas Reader? An Atlas-managed PDF copy will also be removed.`,
    );
  },
  importPdf(file) {
    return request<ImportPdfResult>("/api/library/import", {
      method: "POST",
      body: upload(file),
    });
  },
  queryLibrary(input) {
    return request<LibraryPage>("/api/library/query", {
      method: "POST",
      body: JSON.stringify(input),
    });
  },
  refreshLibrarySources() {
    return request<RefreshSourcesResult>("/api/library/refresh", { method: "POST" });
  },
  relocateDocument(documentId, file) {
    return request<DocumentSummary>(`/api/library/${encodeURIComponent(documentId)}/relocate`, {
      method: "POST",
      body: upload(file),
    });
  },
  removeDocument(documentId) {
    return request<void>(`/api/library/${encodeURIComponent(documentId)}`, { method: "DELETE" });
  },
  async openReader(documentId) {
    const opened = await request<OpenedReaderDocument>("/api/reader/open", {
      method: "POST",
      body: JSON.stringify({ documentId }),
    });
    activeReaderTokens.add(opened.sourceToken);
    return {
      ...opened,
      sourceUrl: `/media/pdf/${encodeURIComponent(opened.sourceToken)}`,
    };
  },
  saveReadingPosition(sourceToken, position) {
    return request<ReadingPosition>("/api/reader/position", {
      method: "POST",
      body: JSON.stringify({ sourceToken, position }),
    });
  },
  closeReader(sourceToken, finalPosition) {
    activeReaderTokens.delete(sourceToken);
    return keepaliveCommand(
      "/api/reader/close",
      "POST",
      JSON.stringify({ sourceToken, finalPosition: finalPosition ?? null }),
    );
  },
  getParsedDocument(documentId) {
    return request<ParsedDocumentView>(`/api/parse/${encodeURIComponent(documentId)}`);
  },
  retryRemoteParse(documentId) {
    return request<ParseSnapshot>(`/api/parse/${encodeURIComponent(documentId)}/retry`, {
      method: "POST",
    });
  },
  async confirmParseReupload() {
    return window.confirm(
      "Cloud MinerU could not confirm the previous upload. Re-uploading may create another billable parse task. Continue?",
    );
  },
  reuploadDocument(documentId, sessionId) {
    return request<ParseSnapshot>(`/api/parse/${encodeURIComponent(documentId)}/reupload`, {
      method: "POST",
      body: JSON.stringify({ sessionId }),
    });
  },
  parseAssetUrl(documentId, artifactId, relativePath) {
    const fileName = relativePath.split("/").at(-1) ?? "";
    const resourceToken = activeTokens?.resourceToken ?? "";
    return `/media/artifacts/${encodeURIComponent(documentId)}/${encodeURIComponent(artifactId)}/images/${encodeURIComponent(fileName)}?access=${encodeURIComponent(resourceToken)}`;
  },
  getProviderSettings() {
    return request<PublicProviderSettings>("/api/providers");
  },
  saveMineruSettings(input) {
    return request<ConnectionTestResult>("/api/providers/mineru", {
      method: "POST",
      body: JSON.stringify(input),
    });
  },
  saveTranslationSettings(input) {
    return request<ConnectionTestResult>("/api/providers/translation", {
      method: "POST",
      body: JSON.stringify(input),
    });
  },
  testProviderConnection(provider) {
    return request<ConnectionTestResult>(`/api/providers/${providerPath(provider)}/test`, {
      method: "POST",
    });
  },
  deleteProviderSecret(provider) {
    return request<void>(`/api/providers/${providerPath(provider)}/secret`, {
      method: "DELETE",
    });
  },
  openReadingSession(input) {
    return request<OpenSessionResult>("/api/sessions/open", {
      method: "POST",
      body: JSON.stringify(input),
    }).then((opened) => {
      activeSessionIds.add(opened.sessionId);
      return opened;
    });
  },
  getReadingSessionSnapshot(sessionId) {
    return request<SessionSnapshot>(`/api/sessions/${encodeURIComponent(sessionId)}`);
  },
  dispatchReadingCommand(input: DispatchReadingCommandInput) {
    return request<CommandReceipt>(
      `/api/sessions/${encodeURIComponent(input.sessionId)}/dispatch`,
      {
        method: "POST",
        body: JSON.stringify({
          commandId: input.commandId,
          expectedRevision: input.expectedRevision ?? null,
          command: input.command,
        }),
      },
    );
  },
  closeReadingSession(sessionId) {
    activeSessionIds.delete(sessionId);
    return keepaliveCommand(`/api/sessions/${encodeURIComponent(sessionId)}`, "DELETE");
  },
};
