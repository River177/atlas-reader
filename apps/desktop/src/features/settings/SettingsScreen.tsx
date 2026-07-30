import type { ConnectionTestResult, PublicProviderSettings } from "@atlas/contracts";

import type { AtlasBridge } from "../../bridge";
import { useProviderSettings, type ProviderForm } from "./useProviderSettings";
import "./settings.css";

interface SettingsScreenProps {
  bridge: AtlasBridge;
}

export function SettingsScreen({ bridge }: SettingsScreenProps) {
  const settings = useProviderSettings(bridge);
  const mineruBusy = settings.busy === "mineru";
  const translationBusy = settings.busy === "translation";

  return (
    <>
      <header className="workspace-header">
        <div>
          <span className="eyebrow">Providers</span>
          <h1>Connect the services Atlas is allowed to call.</h1>
        </div>
      </header>

      {settings.error ? (
        <div className="notice notice--error" role="alert">
          {settings.error}
        </div>
      ) : null}

      <section className="settings-body" aria-label="Provider settings">
        {settings.loading ? (
          <p className="settings-loading">Loading provider settings…</p>
        ) : (
          <>
            <form
              aria-labelledby="mineru-heading"
              className="settings-card"
              onSubmit={(event) => {
                event.preventDefault();
                void settings.saveMineru();
              }}
            >
              <h2 id="mineru-heading">Cloud MinerU</h2>
              <p className="settings-lead">
                Atlas uploads a paper for structure extraction only after you enable automatic cloud
                parsing.
              </p>

              <label htmlFor="mineru-endpoint">Endpoint</label>
              <input
                autoComplete="off"
                disabled={mineruBusy}
                id="mineru-endpoint"
                onChange={(event) =>
                  settings.updateField("mineruEndpoint", event.currentTarget.value)
                }
                placeholder="https://mineru.net/api/v4"
                spellCheck={false}
                type="text"
                value={settings.form.mineruEndpoint}
              />

              <label htmlFor="mineru-api-key">API key</label>
              <input
                autoComplete="off"
                disabled={mineruBusy}
                id="mineru-api-key"
                onChange={(event) =>
                  settings.updateField("mineruApiKey", event.currentTarget.value)
                }
                placeholder={
                  settings.settings.mineruHasSecret
                    ? "Stored in the macOS keychain — leave blank to keep it"
                    : "Paste your Cloud MinerU key"
                }
                spellCheck={false}
                type="password"
                value={settings.form.mineruApiKey}
              />

              <label className="settings-switch" htmlFor="mineru-automatic">
                <input
                  checked={settings.form.mineruAutomaticCloudParsingEnabled}
                  disabled={mineruBusy}
                  id="mineru-automatic"
                  onChange={(event) =>
                    settings.updateField(
                      "mineruAutomaticCloudParsingEnabled",
                      event.currentTarget.checked,
                    )
                  }
                  type="checkbox"
                />
                <span>Parse newly imported papers automatically</span>
              </label>

              <DataEgressDisclosure form={settings.form} stored={settings.settings} />

              <ProviderActions
                busy={mineruBusy}
                hasSecret={settings.settings.mineruHasSecret}
                onDeleteSecret={() => void settings.deleteSecret("mineru")}
                onTest={() => void settings.testConnection("mineru")}
              />
              <ConnectionOutcome label="Cloud MinerU" result={settings.results.mineru} />
            </form>

            <form
              aria-labelledby="translation-heading"
              className="settings-card"
              onSubmit={(event) => {
                event.preventDefault();
                void settings.saveTranslation();
              }}
            >
              <h2 id="translation-heading">Translation model</h2>
              <p className="settings-lead">
                Any OpenAI-compatible endpoint works, including a model server running on this Mac.
              </p>

              <label htmlFor="translation-base-url">Base URL</label>
              <input
                autoComplete="off"
                disabled={translationBusy}
                id="translation-base-url"
                onChange={(event) =>
                  settings.updateField("translationBaseUrl", event.currentTarget.value)
                }
                placeholder="https://api.example.com/v1"
                spellCheck={false}
                type="text"
                value={settings.form.translationBaseUrl}
              />

              <label htmlFor="translation-model-id">Model</label>
              <input
                autoComplete="off"
                disabled={translationBusy}
                id="translation-model-id"
                onChange={(event) =>
                  settings.updateField("translationModelId", event.currentTarget.value)
                }
                placeholder="qwen2.5-14b-instruct"
                spellCheck={false}
                type="text"
                value={settings.form.translationModelId}
              />

              <label htmlFor="translation-api-key">API key</label>
              <input
                autoComplete="off"
                disabled={translationBusy}
                id="translation-api-key"
                onChange={(event) =>
                  settings.updateField("translationApiKey", event.currentTarget.value)
                }
                placeholder={
                  settings.settings.translationHasSecret
                    ? "Stored in the macOS keychain — leave blank to keep it"
                    : "Paste your model API key"
                }
                spellCheck={false}
                type="password"
                value={settings.form.translationApiKey}
              />

              <label htmlFor="translation-context-window">Context window override</label>
              <input
                autoComplete="off"
                disabled={translationBusy}
                id="translation-context-window"
                inputMode="numeric"
                onChange={(event) =>
                  settings.updateField("translationContextWindow", event.currentTarget.value)
                }
                placeholder="Leave blank to use the model default"
                type="text"
                value={settings.form.translationContextWindow}
              />

              <ProviderActions
                busy={translationBusy}
                hasSecret={settings.settings.translationHasSecret}
                onDeleteSecret={() => void settings.deleteSecret("translation")}
                onTest={() => void settings.testConnection("translation")}
              />
              <ConnectionOutcome label="The model endpoint" result={settings.results.translation} />
            </form>
          </>
        )}
      </section>
    </>
  );
}

interface ProviderActionsProps {
  busy: boolean;
  hasSecret: boolean;
  onDeleteSecret(): void;
  onTest(): void;
}

function ProviderActions({ busy, hasSecret, onDeleteSecret, onTest }: ProviderActionsProps) {
  return (
    <div className="settings-actions">
      <button className="primary-action" disabled={busy} type="submit">
        {busy ? "Working…" : "Save and test"}
      </button>
      <button className="secondary-action" disabled={busy} onClick={onTest} type="button">
        Test connection
      </button>
      <button
        className="text-action text-action--danger"
        disabled={busy || !hasSecret}
        onClick={onDeleteSecret}
        type="button"
      >
        Delete stored key
      </button>
      <span className="settings-secret-state">
        {hasSecret ? "Key stored in the macOS keychain" : "No key stored"}
      </span>
    </div>
  );
}

interface ConnectionOutcomeProps {
  label: string;
  result: ConnectionTestResult | undefined;
}

function ConnectionOutcome({ label, result }: ConnectionOutcomeProps) {
  if (!result) {
    return null;
  }
  return (
    <p
      className={`settings-result settings-result--${result.ok ? "ok" : "failed"}`}
      role={result.ok ? "status" : "alert"}
    >
      <strong>{label}:</strong> {result.message}
    </p>
  );
}

interface DataEgressDisclosureProps {
  form: ProviderForm;
  stored: PublicProviderSettings;
}

// Describes the upload the current form would authorise, not the one already
// saved, so the destination on screen is always the destination that would
// receive a paper if the user pressed Save.
function DataEgressDisclosure({ form, stored }: DataEgressDisclosureProps) {
  const endpoint = form.mineruEndpoint.trim();
  const endpointPending = endpoint !== (stored.mineruEndpoint ?? "");
  const automatic = form.mineruAutomaticCloudParsingEnabled;
  const automaticPending = automatic !== stored.mineruAutomaticCloudParsingEnabled;

  return (
    <dl className="settings-disclosure" aria-label="What Atlas sends to Cloud MinerU">
      <div>
        <dt>Destination</dt>
        <dd>
          {endpoint === "" ? "Not configured" : endpoint}
          <Pending show={endpointPending} />
        </dd>
      </div>
      <div>
        <dt>Purpose</dt>
        <dd>Extract the document structure of a paper</dd>
      </div>
      <div>
        <dt>Automatic cloud parsing</dt>
        <dd>
          {automatic ? "On" : "Off"}
          <Pending show={automaticPending} />
        </dd>
      </div>
      <div>
        <dt>Sent when you import a paper</dt>
        <dd>The complete PDF</dd>
      </div>
      <div>
        <dt>Never sent</dt>
        <dd>Local file paths, model keys, and other papers</dd>
      </div>
      <div>
        <dt>Credential</dt>
        <dd>Your own API key, stored in the macOS keychain</dd>
      </div>
    </dl>
  );
}

function Pending({ show }: { show: boolean }) {
  if (!show) {
    return null;
  }
  return <em className="settings-pending"> — not saved yet</em>;
}
