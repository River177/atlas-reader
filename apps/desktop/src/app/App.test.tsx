import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { AtlasBridge } from "../bridge";
import { App } from "./App";

function bridge(): AtlasBridge {
  return {
    queryLibrary: vi.fn().mockResolvedValue({
      items: [],
      nextCursor: null,
    }),
    openReadingSession: vi.fn(),
    dispatchReadingCommand: vi.fn(),
    closeReadingSession: vi.fn(),
  };
}

describe("App", () => {
  it("loads the local library through the bridge", async () => {
    const testBridge = bridge();

    render(<App bridge={testBridge} />);

    expect(await screen.findByText("Your research library is empty")).toBeInTheDocument();
    expect(testBridge.queryLibrary).toHaveBeenCalledWith({
      sort: "recent",
      limit: 30,
    });
  });

  it("surfaces bridge failures without hiding the workspace", async () => {
    const testBridge = bridge();
    vi.mocked(testBridge.queryLibrary).mockRejectedValue(new Error("database unavailable"));

    render(<App bridge={testBridge} />);

    expect(await screen.findByRole("alert")).toHaveTextContent("database unavailable");
    expect(screen.getByText("Local core connected")).toBeInTheDocument();
  });
});
