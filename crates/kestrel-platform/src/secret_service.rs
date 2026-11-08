//! Encryption-key storage in the desktop Secret Service.
//!
//! The key is a Secret Service item in the default collection, found by
//! `application` and `purpose` attributes. The service is asked to unlock or
//! create nothing: a missing or locked keyring is a typed state the caller
//! turns into a memory-only fallback with a remedy.

use std::collections::HashMap;
use std::fmt;

use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zeroize::Zeroizing;

pub const PIN_KEY_BYTES: usize = 32;
/// Secret Service attribute naming this application's items.
pub const KEY_ATTRIBUTE_APPLICATION: &str = kestrel_core::APPLICATION_ID;
/// Secret Service attribute naming what the item is for.
pub const KEY_ATTRIBUTE_PURPOSE: &str = "clipboard-store";

const SERVICE: &str = "org.freedesktop.secrets";
const SERVICE_PATH: &str = "/org/freedesktop/secrets";
const SERVICE_INTERFACE: &str = "org.freedesktop.Secret.Service";
const COLLECTION_INTERFACE: &str = "org.freedesktop.Secret.Collection";
const ITEM_INTERFACE: &str = "org.freedesktop.Secret.Item";
const SESSION_INTERFACE: &str = "org.freedesktop.Secret.Session";
const PROMPT_INTERFACE: &str = "org.freedesktop.Secret.Prompt";
const SESSION_ALGORITHM: &str = "plain";
const DEFAULT_ALIAS: &str = "default";
const ITEM_LABEL: &str = "Kestrel clipboard pin key";
const CONTENT_TYPE: &str = "application/octet-stream";
const PROMPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Why a key operation could not be completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinKeyError {
    /// No Secret Service is available on the session bus.
    Unavailable,
    /// The keyring or its default collection is locked.
    Locked,
    /// The service is reachable but refused the request.
    Failed,
}

impl fmt::Display for PinKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unavailable => "no Secret Service keyring is available",
            Self::Locked => "the Secret Service keyring is locked",
            Self::Failed => "the Secret Service refused the request",
        })
    }
}

impl std::error::Error for PinKeyError {}

/// A 256-bit key; its bytes never appear in `Debug` output.
#[derive(Clone)]
pub struct PinKey(Zeroizing<[u8; PIN_KEY_BYTES]>);

impl PinKey {
    /// Copies `bytes` when they are exactly [`PIN_KEY_BYTES`] long.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let mut key = [0u8; PIN_KEY_BYTES];
        if bytes.len() != PIN_KEY_BYTES {
            return None;
        }
        key.copy_from_slice(bytes);
        Some(Self(Zeroizing::new(key)))
    }

    /// Draws a fresh key from the operating system's random source.
    pub fn generate() -> Result<Self, PinKeyError> {
        let mut key = [0u8; PIN_KEY_BYTES];
        getrandom::fill(&mut key).map_err(|_| PinKeyError::Failed)?;
        Ok(Self(Zeroizing::new(key)))
    }

    pub fn expose(&self) -> &[u8] {
        &self.0[..]
    }
}

impl fmt::Debug for PinKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PinKey(..)")
    }
}

/// Key storage the clipboard persistence layer depends on.
pub trait PinKeyStore: Send + Sync {
    /// The stored key, or `None` when no item exists yet.
    fn get(&self) -> Result<Option<PinKey>, PinKeyError>;
    /// Stores a fresh key; replaces an existing item for this purpose.
    fn create(&self) -> Result<PinKey, PinKeyError>;
    /// Removes the item; `Ok(false)` when it was already absent.
    fn delete(&self) -> Result<bool, PinKeyError>;
}

/// The desktop Secret Service, reached through the session bus.
pub struct SecretServiceKeyStore {
    connection: zbus::blocking::Connection,
}

impl SecretServiceKeyStore {
    /// Connects and confirms the service and a session are usable.
    pub fn connect() -> Result<Self, PinKeyError> {
        let connection =
            zbus::blocking::Connection::session().map_err(|_| PinKeyError::Unavailable)?;
        let store = Self { connection };
        if !store.service_available()? {
            return Err(PinKeyError::Unavailable);
        }
        store.with_session(|_| Ok(()))?;
        Ok(store)
    }

    fn service_available(&self) -> Result<bool, PinKeyError> {
        let dbus = zbus::blocking::fdo::DBusProxy::new(&self.connection)
            .map_err(|_| PinKeyError::Unavailable)?;
        let name = zbus::names::BusName::try_from(SERVICE).map_err(|_| PinKeyError::Unavailable)?;
        if dbus
            .name_has_owner(name)
            .map_err(|_| PinKeyError::Unavailable)?
        {
            return Ok(true);
        }
        dbus.list_activatable_names()
            .map(|names| names.iter().any(|name| name.as_str() == SERVICE))
            .map_err(|_| PinKeyError::Unavailable)
    }

    fn service(&self) -> Result<zbus::blocking::Proxy<'_>, PinKeyError> {
        zbus::blocking::Proxy::new(&self.connection, SERVICE, SERVICE_PATH, SERVICE_INTERFACE)
            .map_err(classify)
    }

    /// Runs `action` under a plain session and closes it afterwards.
    fn with_session<T>(
        &self,
        action: impl FnOnce(&OwnedObjectPath) -> Result<T, PinKeyError>,
    ) -> Result<T, PinKeyError> {
        let service = self.service()?;
        let (_output, session): (OwnedValue, OwnedObjectPath) = service
            .call("OpenSession", &(SESSION_ALGORITHM, Value::new("")))
            .map_err(classify)?;
        if session.as_str() == "/" {
            return Err(PinKeyError::Unavailable);
        }
        let result = action(&session);
        if let Ok(proxy) = zbus::blocking::Proxy::new(
            &self.connection,
            SERVICE,
            session.as_str(),
            SESSION_INTERFACE,
        ) {
            let _ = proxy.call::<_, _, ()>("Close", &());
        }
        result
    }

    fn search(&self) -> Result<(Vec<OwnedObjectPath>, Vec<OwnedObjectPath>), PinKeyError> {
        let service = self.service()?;
        service
            .call("SearchItems", &(attributes(),))
            .map_err(classify)
    }

    fn default_collection(&self) -> Result<OwnedObjectPath, PinKeyError> {
        let service = self.service()?;
        let path: OwnedObjectPath = service
            .call("ReadAlias", &(DEFAULT_ALIAS,))
            .map_err(classify)?;
        if path.as_str() == "/" {
            return Err(PinKeyError::Unavailable);
        }
        Ok(path)
    }

    /// Completes a prompt the service returned, waiting for its signal.
    fn confirm_prompt(&self, prompt: &OwnedObjectPath) -> Result<(), PinKeyError> {
        if prompt.as_str() == "/" {
            return Ok(());
        }
        // The signal iterator blocks; wait from a bounded side thread so a
        // prompt nobody answers cannot strand the caller.
        let connection = self.connection.clone();
        let prompt = prompt.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("kestrel-secret-prompt".to_owned())
            .spawn(move || {
                let result = (|| {
                    let proxy = zbus::blocking::Proxy::new(
                        &connection,
                        SERVICE,
                        prompt.as_str(),
                        PROMPT_INTERFACE,
                    )
                    .map_err(classify)?;
                    let mut completed = proxy.receive_signal("Completed").map_err(classify)?;
                    proxy.call::<_, _, ()>("Prompt", &("",)).map_err(classify)?;
                    let message = completed.next().ok_or(PinKeyError::Failed)?;
                    let (dismissed, _result): (bool, OwnedValue) = message
                        .body()
                        .deserialize()
                        .map_err(|_| PinKeyError::Failed)?;
                    if dismissed {
                        return Err(PinKeyError::Locked);
                    }
                    Ok(())
                })();
                let _ = sender.send(result);
            })
            .map_err(|_| PinKeyError::Failed)?;
        match receiver.recv_timeout(PROMPT_TIMEOUT) {
            Ok(result) => result,
            Err(_) => Err(PinKeyError::Failed),
        }
    }
}

impl PinKeyStore for SecretServiceKeyStore {
    fn get(&self) -> Result<Option<PinKey>, PinKeyError> {
        let (unlocked, locked) = self.search()?;
        if unlocked.is_empty() {
            return if locked.is_empty() {
                Ok(None)
            } else {
                Err(PinKeyError::Locked)
            };
        }
        self.with_session(|session| {
            for path in &unlocked {
                let item = zbus::blocking::Proxy::new(
                    &self.connection,
                    SERVICE,
                    path.as_str(),
                    ITEM_INTERFACE,
                )
                .map_err(classify)?;
                let secret: (OwnedObjectPath, Vec<u8>, Vec<u8>, String) = item
                    .call("GetSecret", &(session.clone(),))
                    .map_err(classify)?;
                let (secret_session, _parameters, value, _content_type) = secret;
                // A plain session returns the bytes as stored; another session
                // means the service transformed them and they are unusable.
                if &secret_session != session {
                    continue;
                }
                if let Some(key) = PinKey::from_bytes(&value) {
                    return Ok(Some(key));
                }
            }
            Ok(None)
        })
    }

    fn create(&self) -> Result<PinKey, PinKeyError> {
        let key = PinKey::generate()?;
        let collection_path = self.default_collection()?;
        self.with_session(|session| {
            let collection = zbus::blocking::Proxy::new(
                &self.connection,
                SERVICE,
                collection_path.as_str(),
                COLLECTION_INTERFACE,
            )
            .map_err(classify)?;
            let mut properties: HashMap<String, Value<'_>> = HashMap::new();
            properties.insert(
                "org.freedesktop.Secret.Item.Label".to_owned(),
                Value::new(ITEM_LABEL),
            );
            properties.insert(
                "org.freedesktop.Secret.Item.Attributes".to_owned(),
                Value::new(attributes()),
            );
            let secret = (
                session.clone(),
                Vec::<u8>::new(),
                key.expose().to_vec(),
                CONTENT_TYPE,
            );
            let (item, prompt): (OwnedObjectPath, OwnedObjectPath) = collection
                .call("CreateItem", &(properties, secret, true))
                .map_err(classify)?;
            self.confirm_prompt(&prompt)?;
            if item.as_str() == "/" {
                return Err(PinKeyError::Failed);
            }
            Ok(key)
        })
    }

    fn delete(&self) -> Result<bool, PinKeyError> {
        let (unlocked, locked) = self.search()?;
        if unlocked.is_empty() {
            return if locked.is_empty() {
                Ok(false)
            } else {
                Err(PinKeyError::Locked)
            };
        }
        let mut deleted = false;
        for path in &unlocked {
            let item = zbus::blocking::Proxy::new(
                &self.connection,
                SERVICE,
                path.as_str(),
                ITEM_INTERFACE,
            )
            .map_err(classify)?;
            let prompt: OwnedObjectPath = item.call("Delete", &()).map_err(classify)?;
            self.confirm_prompt(&prompt)?;
            deleted = true;
        }
        Ok(deleted)
    }
}

fn attributes() -> HashMap<String, String> {
    HashMap::from([
        (
            "application".to_owned(),
            KEY_ATTRIBUTE_APPLICATION.to_owned(),
        ),
        ("purpose".to_owned(), KEY_ATTRIBUTE_PURPOSE.to_owned()),
    ])
}

fn classify(error: zbus::Error) -> PinKeyError {
    match error {
        zbus::Error::MethodError(name, _, _) => match name.as_str() {
            "org.freedesktop.Secret.Error.IsLocked" => PinKeyError::Locked,
            "org.freedesktop.DBus.Error.ServiceUnknown"
            | "org.freedesktop.DBus.Error.NameHasNoOwner" => PinKeyError::Unavailable,
            _ => PinKeyError::Failed,
        },
        zbus::Error::InputOutput(_) => PinKeyError::Unavailable,
        _ => PinKeyError::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::{PIN_KEY_BYTES, PinKey, PinKeyError};

    #[test]
    fn keys_require_the_exact_length_and_never_debug_their_bytes() {
        assert!(PinKey::from_bytes(&[1u8; PIN_KEY_BYTES - 1]).is_none());
        assert!(PinKey::from_bytes(&[1u8; PIN_KEY_BYTES + 1]).is_none());
        let key = PinKey::from_bytes(&[7u8; PIN_KEY_BYTES]).expect("exact length");
        assert_eq!(key.expose(), &[7u8; PIN_KEY_BYTES]);
        assert_eq!(format!("{key:?}"), "PinKey(..)");
        assert_eq!(format!("{:?}", PinKeyError::Unavailable), "Unavailable");
    }

    #[test]
    fn generated_keys_differ() {
        let first = PinKey::generate().expect("operating-system randomness");
        let second = PinKey::generate().expect("operating-system randomness");
        assert_ne!(first.expose(), second.expose());
    }

    #[test]
    fn real_secret_service_round_trip() {
        if std::env::var("KESTREL_SECRET_SERVICE_TEST").as_deref() != Ok("1") {
            return;
        }
        use super::{PinKeyStore, SecretServiceKeyStore};

        let store = match SecretServiceKeyStore::connect() {
            Ok(store) => store,
            Err(error) => panic!("connect failed: {error:?}"),
        };
        let _ = store.delete();
        let created = match store.create() {
            Ok(key) => key,
            Err(error) => panic!("create failed: {error:?}"),
        };
        let second = match SecretServiceKeyStore::connect() {
            Ok(store) => store,
            Err(error) => panic!("second connect failed: {error:?}"),
        };
        let loaded = match second.get() {
            Ok(Some(key)) => key,
            Ok(None) => panic!("the created item was not found"),
            Err(error) => panic!("get failed: {error:?}"),
        };
        assert_eq!(created.expose(), loaded.expose());
        assert_eq!(second.delete(), Ok(true));
        assert!(second.get().expect("get after delete").is_none());
    }
}
