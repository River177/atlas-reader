import { beforeEach, describe, expect, it, vi } from "vitest";

describe("browserBridge", () => {
  beforeEach(() => {
    vi.resetModules();
    window.sessionStorage.clear();
    window.history.replaceState(null, "", "/");
  });

  it("exchanges the launch fragment once and attaches CSRF to commands", async () => {
    window.history.replaceState(null, "", "/#launch=launch-token");
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            accessToken: "access-token",
            csrfToken: "csrf-token",
            resourceToken: "resource-token",
          }),
          {
            status: 200,
            headers: { "content-type": "application/json" },
          },
        ),
      )
      .mockResolvedValueOnce(
        new Response(JSON.stringify({ items: [], nextCursor: null }), {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
      );
    vi.stubGlobal("fetch", fetchMock);
    const { browserBridge } = await import("./browser-bridge");

    await browserBridge.queryLibrary({ sort: "recent", limit: 100 });

    expect(fetchMock).toHaveBeenNthCalledWith(
      1,
      "/api/bootstrap/exchange",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({ launchToken: "launch-token" }),
      }),
    );
    const command = fetchMock.mock.calls[1];
    expect(command?.[0]).toBe("/api/library/query");
    const headers = new Headers(command?.[1]?.headers);
    expect(headers.get("authorization")).toBe("Bearer access-token");
    expect(headers.get("x-atlas-client")).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-[45][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
    );
    expect(headers.get("x-atlas-csrf")).toBe("csrf-token");
    expect(window.location.hash).toBe("");
  });

  it("surfaces structured Atlas errors from HTTP responses", async () => {
    window.history.replaceState(null, "", "/#launch=launch-token");
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            accessToken: "access-token",
            csrfToken: "csrf-token",
            resourceToken: "resource-token",
          }),
          {
            status: 200,
            headers: { "content-type": "application/json" },
          },
        ),
      )
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            code: "storage_unavailable",
            message: "database unavailable",
            recoverable: true,
          }),
          { status: 503, headers: { "content-type": "application/json" } },
        ),
      );
    vi.stubGlobal("fetch", fetchMock);
    const { browserBridge } = await import("./browser-bridge");

    await expect(browserBridge.getProviderSettings()).rejects.toEqual(
      expect.objectContaining({ message: "database unavailable" }),
    );
  });

  it("assigns duplicated browsing contexts independent lease identities", async () => {
    window.history.replaceState(null, "", "/#launch=launch-token");
    const tokens = {
      accessToken: "access-token",
      csrfToken: "csrf-token",
      resourceToken: "resource-token",
    };
    const page = { items: [], nextCursor: null };
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(new Response(JSON.stringify(tokens), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify(page), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify(tokens), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify(page), { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);

    const first = await import("./browser-bridge");
    await first.browserBridge.queryLibrary({ sort: "recent", limit: 100 });
    const firstClient = new Headers(fetchMock.mock.calls[1]?.[1]?.headers).get("x-atlas-client");

    vi.resetModules();
    const second = await import("./browser-bridge");
    await second.browserBridge.queryLibrary({ sort: "recent", limit: 100 });
    const resumeHeaders = new Headers(fetchMock.mock.calls[2]?.[1]?.headers);
    const secondClient = new Headers(fetchMock.mock.calls[3]?.[1]?.headers).get("x-atlas-client");

    expect(resumeHeaders.get("x-atlas-client")).toBe(secondClient);
    expect(firstClient).toBeTruthy();
    expect(secondClient).toBeTruthy();
    expect(secondClient).not.toBe(firstClient);
  });
});
