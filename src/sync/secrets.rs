//! 端末トークンの保存先。設定ファイルや DB には置かず、OS のキーチェーン(macOS)/資格情報マネージャー(Windows)に置く。

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Mutex;

pub trait TokenStore {
    fn get(&self, server: &str, scope: &str) -> Result<Option<String>>;
    fn set(&self, server: &str, scope: &str, token: &str) -> Result<()>;
    fn delete(&self, server: &str, scope: &str) -> Result<()>;
}

const SERVICE: &str = "SyncLane";

pub struct KeyringStore;

impl KeyringStore {
    fn entry(server: &str, scope: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(SERVICE, &format!("{server}#{scope}")).context("failed to open the keychain entry")
    }
}

impl TokenStore for KeyringStore {
    fn get(&self, server: &str, scope: &str) -> Result<Option<String>> {
        match Self::entry(server, scope)?.get_password() {
            Ok(token) => Ok(Some(token)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error).context("failed to read the token from the keychain"),
        }
    }

    fn set(&self, server: &str, scope: &str, token: &str) -> Result<()> {
        Self::entry(server, scope)?
            .set_password(token)
            .context("failed to save the token in the keychain")
    }

    fn delete(&self, server: &str, scope: &str) -> Result<()> {
        match Self::entry(server, scope)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error).context("failed to delete the token from the keychain"),
        }
    }
}

/// メモリの上だけの保存先(テスト用)。
#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<(String, String), String>>);

impl TokenStore for MemoryStore {
    fn get(&self, server: &str, scope: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(&(server.to_string(), scope.to_string())).cloned())
    }

    fn set(&self, server: &str, scope: &str, token: &str) -> Result<()> {
        self.0.lock().unwrap().insert((server.to_string(), scope.to_string()), token.to_string());
        Ok(())
    }

    fn delete(&self, server: &str, scope: &str) -> Result<()> {
        self.0.lock().unwrap().remove(&(server.to_string(), scope.to_string()));
        Ok(())
    }
}
