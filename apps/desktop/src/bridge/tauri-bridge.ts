import type { CommandReceipt, LibraryPage, OpenSessionResult } from "@atlas/contracts";
import { invoke } from "@tauri-apps/api/core";

import type { AtlasBridge, DispatchReadingCommandInput } from "./atlas-bridge";

export const tauriBridge: AtlasBridge = {
  queryLibrary(input) {
    return invoke<LibraryPage>("library_query", { input });
  },
  openReadingSession(input) {
    return invoke<OpenSessionResult>("reading_session_open", { input });
  },
  dispatchReadingCommand(input: DispatchReadingCommandInput) {
    return invoke<CommandReceipt>("reading_session_dispatch", { input });
  },
  closeReadingSession(sessionId) {
    return invoke<void>("reading_session_close", { sessionId });
  },
};
