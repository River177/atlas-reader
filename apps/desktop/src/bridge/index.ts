import type { AtlasBridge } from "./atlas-bridge";
import { browserBridge } from "./browser-bridge";
import { tauriBridge } from "./tauri-bridge";

const isTauriRuntime = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export const atlasBridge: AtlasBridge = isTauriRuntime ? tauriBridge : browserBridge;

export type {
  AtlasBridge,
  OpenedReaderView,
  PdfDropEvent,
} from "./atlas-bridge";
