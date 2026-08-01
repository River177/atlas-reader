import type { AtlasBridge } from "./atlas-bridge";
import { browserBridge } from "./browser-bridge";

export const atlasBridge: AtlasBridge = browserBridge;

export type { AtlasBridge, OpenedReaderView, PdfDropEvent } from "./atlas-bridge";
