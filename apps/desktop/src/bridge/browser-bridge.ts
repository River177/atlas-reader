import type { AtlasBridge } from "./atlas-bridge";

function unavailable(method: string): never {
  throw new Error(`${method} is only available inside the Atlas Reader desktop app`);
}

export const browserBridge: AtlasBridge = {
  async queryLibrary() {
    return {
      items: [],
      nextCursor: null,
    };
  },
  async openReadingSession() {
    return unavailable("openReadingSession");
  },
  async dispatchReadingCommand() {
    return unavailable("dispatchReadingCommand");
  },
  async closeReadingSession() {
    return unavailable("closeReadingSession");
  },
};
