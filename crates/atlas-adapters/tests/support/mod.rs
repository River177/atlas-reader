use std::path::PathBuf;

use atlas_adapters::MacOsKeychainAdapter;
use atlas_domain::{AtlasError, ProviderKind};
use atlas_provider_settings::{Secret, SecretStore, secret_account, secret_env_var};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

pub async fn effective_provider_secret(kind: ProviderKind) -> Result<Option<Secret>, AtlasError> {
    if let Some(variable) = secret_env_var(&secret_account(kind))
        && let Ok(value) = std::env::var(variable)
        && !value.trim().is_empty()
    {
        return Ok(Some(Secret::new(value.trim())));
    }

    let account = configured_account(kind)
        .await
        .unwrap_or_else(|| secret_account(kind));
    MacOsKeychainAdapter::new().get(&account)
}

async fn configured_account(kind: ProviderKind) -> Option<String> {
    let path = database_path()?;
    if !path.is_file() {
        return None;
    }
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .create_if_missing(false);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .ok()?;
    let account = sqlx::query_scalar::<_, String>(
        "SELECT secret_account
         FROM provider_profiles
         WHERE kind = ?1
         LIMIT 1",
    )
    .bind(kind.as_str())
    .fetch_optional(&pool)
    .await
    .ok()
    .flatten();
    pool.close().await;
    account
}

fn database_path() -> Option<PathBuf> {
    std::env::var_os("ATLAS_APP_DATABASE_PATH")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| {
                PathBuf::from(home)
                    .join("Library/Application Support/com.atlasreader.desktop/atlas.sqlite3")
            })
        })
}
