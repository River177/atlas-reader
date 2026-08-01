import { useCallback, useEffect, useState } from "react";
import type { ConnectionTestResult, ProviderKind, PublicProviderSettings } from "@atlas/contracts";

import type { AtlasBridge } from "../../bridge";
import { errorMessage } from "../../bridge/error-message";

export interface ProviderForm {
  mineruEndpoint: string;
  mineruApiKey: string;
  mineruAutomaticCloudParsingEnabled: boolean;
  translationBaseUrl: string;
  translationApiKey: string;
  translationModelId: string;
  translationContextWindow: string;
}

export type ProviderFormField = keyof ProviderForm;

/// What the operation did, which decides how much of the form may be replaced.
type CommitIntent = "save" | "test" | "delete";

export type ConnectionResults = Partial<Record<ProviderKind, ConnectionTestResult>>;

/// Mirrors the bounds `atlas-provider-settings` enforces.
const MIN_CONTEXT_WINDOW = 1_024;
const MAX_CONTEXT_WINDOW = 8_000_000;

const emptySettings: PublicProviderSettings = {
  mineruEndpoint: null,
  mineruHasSecret: false,
  mineruAutomaticCloudParsingEnabled: false,
  translationBaseUrl: null,
  translationModelId: null,
  translationHasSecret: false,
  contextWindowOverride: null,
};

// Owns every rule the settings screen needs: what the form shows before the
// stored settings arrive, when a blank API key means "keep the stored one", and
// how a completed save changes the form.
export function useProviderSettings(bridge: AtlasBridge) {
  const [settings, setSettings] = useState<PublicProviderSettings>(emptySettings);
  const [form, setForm] = useState<ProviderForm>(() => formFromSettings(emptySettings));
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<ProviderKind>();
  const [results, setResults] = useState<ConnectionResults>({});
  const [error, setError] = useState<string>();

  useEffect(() => {
    let active = true;
    void (async () => {
      try {
        const loaded = await bridge.getProviderSettings();
        if (!active) {
          return;
        }
        setSettings(loaded);
        setForm(formFromSettings(loaded));
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

  const updateField = useCallback(
    <Field extends ProviderFormField>(field: Field, value: ProviderForm[Field]) => {
      setForm((current) => ({ ...current, [field]: value }));
    },
    [],
  );

  const run = useCallback(
    async (
      provider: ProviderKind,
      intent: CommitIntent,
      operation: () => Promise<ConnectionTestResult | undefined>,
    ) => {
      setBusy(provider);
      setError(undefined);
      try {
        const result = await operation();
        const reloaded = await bridge.getProviderSettings();
        setSettings(reloaded);
        setForm((current) => afterCommit(current, reloaded, provider, intent));
        setResults((current) => ({ ...current, [provider]: result }));
      } catch (reason) {
        setError(errorMessage(reason));
      } finally {
        setBusy(undefined);
      }
    },
    [bridge],
  );

  const saveMineru = useCallback(
    () =>
      run("mineru", "save", () => {
        const apiKey = form.mineruApiKey.trim();
        return bridge.saveMineruSettings({
          endpoint: form.mineruEndpoint.trim(),
          apiKey: apiKey === "" ? null : apiKey,
          automaticCloudParsingEnabled: form.mineruAutomaticCloudParsingEnabled,
        });
      }),
    [bridge, form, run],
  );

  const saveTranslation = useCallback(async () => {
    const contextWindow = parseContextWindow(form.translationContextWindow);
    if (contextWindow === "invalid") {
      setError(
        `The context window override must be a whole number between ${MIN_CONTEXT_WINDOW} and ${MAX_CONTEXT_WINDOW} tokens`,
      );
      return;
    }
    await run("translation", "save", () => {
      const apiKey = form.translationApiKey.trim();
      return bridge.saveTranslationSettings({
        baseUrl: form.translationBaseUrl.trim(),
        apiKey: apiKey === "" ? null : apiKey,
        modelId: form.translationModelId.trim(),
        contextWindowOverride: contextWindow,
      });
    });
  }, [bridge, form, run]);

  const testConnection = useCallback(
    (provider: ProviderKind) =>
      run(provider, "test", () => bridge.testProviderConnection(provider)),
    [bridge, run],
  );

  const deleteSecret = useCallback(
    (provider: ProviderKind) =>
      run(provider, "delete", async () => {
        await bridge.deleteProviderSecret(provider);
        return undefined;
      }),
    [bridge, run],
  );

  return {
    loading,
    error,
    settings,
    form,
    busy,
    results,
    updateField,
    saveMineru,
    saveTranslation,
    testConnection,
    deleteSecret,
  };
}

function formFromSettings(settings: PublicProviderSettings): ProviderForm {
  return {
    mineruEndpoint: settings.mineruEndpoint ?? "",
    mineruApiKey: "",
    mineruAutomaticCloudParsingEnabled: settings.mineruAutomaticCloudParsingEnabled,
    translationBaseUrl: settings.translationBaseUrl ?? "",
    translationApiKey: "",
    translationModelId: settings.translationModelId ?? "",
    translationContextWindow:
      settings.contextWindowOverride === null ? "" : String(settings.contextWindowOverride),
  };
}

// Keeps whatever the user is still editing. Only the key that was just stored is
// cleared, and only the provider that was just changed re-reads the switch the
// core owns, so an operation on one provider never discards work on the other.
function afterCommit(
  current: ProviderForm,
  settings: PublicProviderSettings,
  provider: ProviderKind,
  intent: CommitIntent,
): ProviderForm {
  if (intent === "test") {
    return current;
  }
  const next = { ...current };
  if (intent === "save") {
    if (provider === "mineru") {
      next.mineruApiKey = "";
    } else {
      next.translationApiKey = "";
    }
  }
  if (provider === "mineru") {
    next.mineruAutomaticCloudParsingEnabled = settings.mineruAutomaticCloudParsingEnabled;
  }
  return next;
}

function parseContextWindow(value: string): number | null | "invalid" {
  const trimmed = value.trim();
  if (trimmed === "") {
    return null;
  }
  if (!/^\d+$/.test(trimmed)) {
    return "invalid";
  }
  const parsed = Number(trimmed);
  if (!Number.isSafeInteger(parsed)) {
    return "invalid";
  }
  // Matches the bounds the core enforces, so the user hears about it before a
  // round trip rather than after one.
  return parsed >= MIN_CONTEXT_WINDOW && parsed <= MAX_CONTEXT_WINDOW ? parsed : "invalid";
}
