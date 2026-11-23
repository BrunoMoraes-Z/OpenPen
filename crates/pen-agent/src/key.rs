//! Anthropic API key storage: the OS credential store (Windows Credential
//! Manager, macOS Keychain, Linux keyutils), with the `ANTHROPIC_API_KEY`
//! environment variable as a fallback. The key is never logged or put in an
//! error message.

const SERVICE: &str = "OpenPen";
const USER: &str = "anthropic-api-key";
pub const ENV_VAR: &str = "ANTHROPIC_API_KEY";

/// Where a key came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    CredentialStore,
    Environment,
}

fn entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, USER).map_err(|e| format!("credential store unavailable: {e}"))
}

/// Reads the stored key (credential store only).
pub fn get_stored_key() -> Result<Option<String>, String> {
    match entry()?.get_password() {
        Ok(k) if !k.trim().is_empty() => Ok(Some(k)),
        Ok(_) | Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("could not read the API key: {e}")),
    }
}

/// Saves the key in the credential store.
pub fn set_stored_key(key: &str) -> Result<(), String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("the API key is empty".into());
    }
    entry()?
        .set_password(key)
        .map_err(|e| format!("could not save the API key: {e}"))
}

/// Removes the stored key (no error if there was none).
pub fn delete_stored_key() -> Result<(), String> {
    match entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("could not delete the API key: {e}")),
    }
}

/// The key to use: the stored one, else `ANTHROPIC_API_KEY`.
pub fn resolve_api_key() -> Option<(String, KeySource)> {
    if let Ok(Some(k)) = get_stored_key() {
        return Some((k, KeySource::CredentialStore));
    }
    std::env::var(ENV_VAR)
        .ok()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .map(|k| (k, KeySource::Environment))
}

/// `sk-ant-…wxyz` for display.
pub fn mask_key(key: &str) -> String {
    let k = key.trim();
    let tail: String = k
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if k.chars().count() <= 12 {
        return "…".to_string() + &tail;
    }
    let head: String = k.chars().take(7).collect();
    format!("{head}…{tail}")
}

/// Wraps a secret so it never shows up in `Debug` output.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into().trim().to_string())
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(***)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masking() {
        assert_eq!(
            mask_key("sk-ant-api03-abcdefghijklmnop-wxyz"),
            "sk-ant-…wxyz"
        );
        assert_eq!(mask_key("short"), "…hort");
        assert_eq!(format!("{:?}", ApiKey::new("secret")), "ApiKey(***)");
    }
}
