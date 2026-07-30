import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { ConnectionTestResult, PublicProviderSettings } from "@atlas/contracts";

import type { AtlasBridge } from "../../bridge";
import { testBridge } from "../../test/bridge";
import { SettingsScreen } from "./SettingsScreen";

function settings(overrides: Partial<PublicProviderSettings> = {}): PublicProviderSettings {
  return {
    mineruEndpoint: null,
    mineruHasSecret: false,
    mineruAutomaticCloudParsingEnabled: false,
    translationBaseUrl: null,
    translationModelId: null,
    translationHasSecret: false,
    contextWindowOverride: null,
    ...overrides,
  };
}

function ok(message: string): ConnectionTestResult {
  return { ok: true, code: "ok", message };
}

function failed(code: ConnectionTestResult["code"], message: string): ConnectionTestResult {
  return { ok: false, code, message };
}

async function renderSettings(bridge: AtlasBridge) {
  render(<SettingsScreen bridge={bridge} />);
  await waitFor(() => {
    expect(screen.queryByText("Loading provider settings…")).toBeNull();
  });
}

function type(field: HTMLElement, value: string) {
  fireEvent.change(field, { target: { value } });
}

function apiKeyField(index: number): HTMLElement {
  const fields = screen.getAllByLabelText("API key");
  const field = fields[index];
  if (!field) {
    throw new Error(`no API key field at index ${index}`);
  }
  return field;
}

function action(name: string, index: number): HTMLElement {
  const buttons = screen.getAllByRole("button", { name });
  const button = buttons[index];
  if (!button) {
    throw new Error(`no "${name}" button at index ${index}`);
  }
  return button;
}

describe("SettingsScreen", () => {
  it("shows the stored endpoint and model without ever echoing a key", async () => {
    const bridge = testBridge();
    bridge.getProviderSettings = vi.fn().mockResolvedValue(
      settings({
        mineruEndpoint: "https://mineru.net/api/v4",
        mineruHasSecret: true,
        mineruAutomaticCloudParsingEnabled: true,
        translationBaseUrl: "https://api.example.com/v1",
        translationModelId: "qwen2.5-14b-instruct",
        translationHasSecret: true,
        contextWindowOverride: 32_000,
      }),
    );

    await renderSettings(bridge);

    expect(screen.getByLabelText("Endpoint")).toHaveValue("https://mineru.net/api/v4");
    expect(screen.getByLabelText("Base URL")).toHaveValue("https://api.example.com/v1");
    expect(screen.getByLabelText("Model")).toHaveValue("qwen2.5-14b-instruct");
    expect(screen.getByLabelText("Context window override")).toHaveValue("32000");
    expect(screen.getByLabelText("Parse newly imported papers automatically")).toBeChecked();
    for (const field of screen.getAllByLabelText("API key")) {
      expect(field).toHaveValue("");
      expect(field).toHaveAttribute(
        "placeholder",
        "Stored in the macOS keychain — leave blank to keep it",
      );
    }
    expect(screen.getAllByText("Key stored in the macOS keychain")).toHaveLength(2);
  });

  it("saves the MinerU endpoint with the typed key and reports success", async () => {
    const bridge = testBridge();
    bridge.saveMineruSettings = vi.fn().mockResolvedValue(ok("Cloud MinerU accepted the key."));
    bridge.getProviderSettings = vi
      .fn()
      .mockResolvedValueOnce(settings())
      .mockResolvedValue(
        settings({ mineruEndpoint: "https://mineru.net/api/v4", mineruHasSecret: true }),
      );

    await renderSettings(bridge);
    type(screen.getByLabelText("Endpoint"), "https://mineru.net/api/v4");
    type(apiKeyField(0), "secret-key");
    fireEvent.click(action("Save and test", 0));

    await waitFor(() => {
      expect(bridge.saveMineruSettings).toHaveBeenCalledWith({
        endpoint: "https://mineru.net/api/v4",
        apiKey: "secret-key",
        automaticCloudParsingEnabled: false,
      });
    });
    expect(await screen.findByText(/Cloud MinerU accepted the key\./)).toBeVisible();
    expect(apiKeyField(0)).toHaveValue("");
  });

  it("sends no key when the field is left blank so the stored key survives", async () => {
    const bridge = testBridge();
    bridge.getProviderSettings = vi
      .fn()
      .mockResolvedValue(
        settings({ mineruEndpoint: "https://mineru.net/api/v4", mineruHasSecret: true }),
      );
    bridge.saveMineruSettings = vi.fn().mockResolvedValue(ok("Cloud MinerU accepted the key."));

    await renderSettings(bridge);
    fireEvent.click(screen.getByLabelText("Parse newly imported papers automatically"));
    fireEvent.click(action("Save and test", 0));

    await waitFor(() => {
      expect(bridge.saveMineruSettings).toHaveBeenCalledWith({
        endpoint: "https://mineru.net/api/v4",
        apiKey: null,
        automaticCloudParsingEnabled: true,
      });
    });
  });

  it("surfaces a rejected URL as an alert and keeps the typed value", async () => {
    const bridge = testBridge();
    bridge.saveMineruSettings = vi
      .fn()
      .mockResolvedValue(
        failed("insecure_remote_url", "Only a local endpoint may use plain HTTP."),
      );

    await renderSettings(bridge);
    type(screen.getByLabelText("Endpoint"), "http://mineru.net/api/v4");
    fireEvent.click(action("Save and test", 0));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Only a local endpoint may use plain HTTP.");
    expect(screen.getByLabelText("Endpoint")).toHaveValue("http://mineru.net/api/v4");
  });

  it("rejects a context window the core would refuse before calling it", async () => {
    const bridge = testBridge();

    await renderSettings(bridge);
    for (const value of ["32k", "512", "9000000"]) {
      type(screen.getByLabelText("Context window override"), value);
      fireEvent.click(action("Save and test", 1));

      expect(await screen.findByRole("alert")).toHaveTextContent(
        "The context window override must be a whole number between 1024 and 8000000 tokens",
      );
      expect(bridge.saveTranslationSettings).not.toHaveBeenCalled();
    }
  });

  it("keeps a key being typed for one provider while another provider is saved", async () => {
    const bridge = testBridge();
    bridge.saveMineruSettings = vi.fn().mockResolvedValue(ok("Cloud MinerU accepted the key."));

    await renderSettings(bridge);
    type(apiKeyField(1), "translation-key-in-progress");
    type(screen.getByLabelText("Endpoint"), "https://mineru.net/api/v4");
    type(apiKeyField(0), "mineru-key");
    fireEvent.click(action("Save and test", 0));

    await waitFor(() => {
      expect(bridge.saveMineruSettings).toHaveBeenCalled();
    });
    expect(apiKeyField(0)).toHaveValue("");
    expect(apiKeyField(1)).toHaveValue("translation-key-in-progress");
  });

  it("keeps an unsaved automatic-parsing choice while a connection is tested", async () => {
    const bridge = testBridge();
    bridge.getProviderSettings = vi
      .fn()
      .mockResolvedValue(
        settings({ mineruEndpoint: "https://mineru.net/api/v4", mineruHasSecret: true }),
      );
    bridge.testProviderConnection = vi.fn().mockResolvedValue(ok("Cloud MinerU answered."));

    await renderSettings(bridge);
    fireEvent.click(screen.getByLabelText("Parse newly imported papers automatically"));
    fireEvent.click(action("Test connection", 0));

    await waitFor(() => {
      expect(bridge.testProviderConnection).toHaveBeenCalledWith("mineru");
    });
    expect(screen.getByLabelText("Parse newly imported papers automatically")).toBeChecked();
  });

  it("saves the translation provider with a parsed context window", async () => {
    const bridge = testBridge();
    bridge.saveTranslationSettings = vi.fn().mockResolvedValue(ok("The model endpoint answered."));

    await renderSettings(bridge);
    type(screen.getByLabelText("Base URL"), "https://api.example.com/v1");
    type(screen.getByLabelText("Model"), "qwen2.5-14b-instruct");
    type(apiKeyField(1), "model-key");
    type(screen.getByLabelText("Context window override"), "32000");
    fireEvent.click(action("Save and test", 1));

    await waitFor(() => {
      expect(bridge.saveTranslationSettings).toHaveBeenCalledWith({
        baseUrl: "https://api.example.com/v1",
        apiKey: "model-key",
        modelId: "qwen2.5-14b-instruct",
        contextWindowOverride: 32_000,
      });
    });
    expect(await screen.findByText(/The model endpoint answered\./)).toBeVisible();
  });

  it("tests a stored provider without resubmitting the form", async () => {
    const bridge = testBridge();
    bridge.getProviderSettings = vi
      .fn()
      .mockResolvedValue(
        settings({ mineruEndpoint: "https://mineru.net/api/v4", mineruHasSecret: true }),
      );
    bridge.testProviderConnection = vi
      .fn()
      .mockResolvedValue(failed("rate_limited", "Cloud MinerU is rate limiting this key."));

    await renderSettings(bridge);
    fireEvent.click(action("Test connection", 0));

    await waitFor(() => {
      expect(bridge.testProviderConnection).toHaveBeenCalledWith("mineru");
    });
    expect(bridge.saveMineruSettings).not.toHaveBeenCalled();
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Cloud MinerU is rate limiting this key.",
    );
  });

  it("deletes a stored key and reflects the cleared state", async () => {
    const bridge = testBridge();
    bridge.getProviderSettings = vi
      .fn()
      .mockResolvedValueOnce(
        settings({
          mineruEndpoint: "https://mineru.net/api/v4",
          mineruHasSecret: true,
          mineruAutomaticCloudParsingEnabled: true,
        }),
      )
      .mockResolvedValue(settings({ mineruEndpoint: "https://mineru.net/api/v4" }));

    await renderSettings(bridge);
    fireEvent.click(action("Delete stored key", 0));

    await waitFor(() => {
      expect(bridge.deleteProviderSecret).toHaveBeenCalledWith("mineru");
    });
    await waitFor(() => {
      expect(screen.getByLabelText("Parse newly imported papers automatically")).not.toBeChecked();
    });
    expect(action("Delete stored key", 0)).toBeDisabled();
  });

  it("does not offer to delete a key that was never stored", async () => {
    const bridge = testBridge();

    await renderSettings(bridge);

    for (const button of screen.getAllByRole("button", { name: "Delete stored key" })) {
      expect(button).toBeDisabled();
    }
    expect(screen.getAllByText("No key stored")).toHaveLength(2);
  });

  it("names the destination and the payload before any upload can happen", async () => {
    const bridge = testBridge();
    bridge.getProviderSettings = vi
      .fn()
      .mockResolvedValue(settings({ mineruEndpoint: "https://mineru.net/api/v4" }));

    await renderSettings(bridge);

    const disclosure = screen.getByLabelText("What Atlas sends to Cloud MinerU");
    expect(disclosure).toHaveTextContent("https://mineru.net/api/v4");
    expect(disclosure).toHaveTextContent("The complete PDF");
    expect(disclosure).toHaveTextContent("Local file paths, model keys, and other papers");
    expect(disclosure).toHaveTextContent("Off");
    expect(disclosure).not.toHaveTextContent("not saved yet");
  });

  it("names the endpoint that would actually receive the upload", async () => {
    const bridge = testBridge();
    bridge.getProviderSettings = vi
      .fn()
      .mockResolvedValue(settings({ mineruEndpoint: "https://mineru.net/api/v4" }));

    await renderSettings(bridge);
    type(screen.getByLabelText("Endpoint"), "https://replacement.example.com/api/v4");
    fireEvent.click(screen.getByLabelText("Parse newly imported papers automatically"));

    const disclosure = screen.getByLabelText("What Atlas sends to Cloud MinerU");
    expect(disclosure).toHaveTextContent("https://replacement.example.com/api/v4");
    expect(disclosure).not.toHaveTextContent("https://mineru.net/api/v4");
    expect(disclosure).toHaveTextContent("On");
    expect(disclosure.textContent?.match(/not saved yet/g)).toHaveLength(2);
  });

  it("reports a failure to load the stored settings", async () => {
    const bridge = testBridge();
    bridge.getProviderSettings = vi.fn().mockRejectedValue(new Error("keychain is locked"));

    render(<SettingsScreen bridge={bridge} />);

    expect(await screen.findByRole("alert")).toHaveTextContent("keychain is locked");
  });
});
