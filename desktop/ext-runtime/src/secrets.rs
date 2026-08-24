use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::Instant;

use shilpo_ext_api::{ExtensionId, SecretPurpose, SecretRef};

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum SecretBrokerError {
    BackendUnavailable(String),
    Locked(String),
    Denied(String),
    Cancelled(String),
    NotFound(String),
    InvalidReference(String),
    Internal(String),
}

impl fmt::Display for SecretBrokerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BackendUnavailable(msg) => write!(f, "secret backend unavailable: {msg}"),
            Self::Locked(msg) => write!(f, "secret storage locked: {msg}"),
            Self::Denied(msg) => write!(f, "secret access denied: {msg}"),
            Self::Cancelled(msg) => write!(f, "secret operation cancelled: {msg}"),
            Self::NotFound(msg) => write!(f, "secret not found: {msg}"),
            Self::InvalidReference(msg) => write!(f, "invalid secret reference: {msg}"),
            Self::Internal(msg) => write!(f, "secret broker internal error: {msg}"),
        }
    }
}

impl std::error::Error for SecretBrokerError {}

pub trait SecretBroker: Send + Sync {
    fn set(
        &self,
        extension_id: &ExtensionId,
        purpose: &SecretPurpose,
        value: &[u8],
        deadline: Instant,
    ) -> Result<SecretRef, SecretBrokerError>;

    fn read(
        &self,
        extension_id: &ExtensionId,
        purpose: &SecretPurpose,
        reference: &SecretRef,
        deadline: Instant,
    ) -> Result<Option<Vec<u8>>, SecretBrokerError>;

    fn delete(
        &self,
        extension_id: &ExtensionId,
        purpose: &SecretPurpose,
        reference: &SecretRef,
        deadline: Instant,
    ) -> Result<(), SecretBrokerError>;

    fn delete_all(
        &self,
        extension_id: &ExtensionId,
        deadline: Instant,
    ) -> Result<(), SecretBrokerError>;
}

/// Hermetic in-memory secret broker for unit tests and local testing.
#[derive(Default)]
pub struct FakeSecretBroker {
    storage: RwLock<HashMap<(ExtensionId, SecretPurpose, String), Vec<u8>>>,
    simulated_error: RwLock<Option<SecretBrokerError>>,
}

impl FakeSecretBroker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_simulated_error(&self, error: Option<SecretBrokerError>) {
        let mut guard = self.simulated_error.write().unwrap();
        *guard = error;
    }

    fn check_simulated_error(&self) -> Result<(), SecretBrokerError> {
        let guard = self.simulated_error.read().unwrap();
        if let Some(err) = guard.as_ref() {
            Err(err.clone())
        } else {
            Ok(())
        }
    }
}

impl SecretBroker for FakeSecretBroker {
    fn set(
        &self,
        extension_id: &ExtensionId,
        purpose: &SecretPurpose,
        value: &[u8],
        deadline: Instant,
    ) -> Result<SecretRef, SecretBrokerError> {
        ensure_deadline(deadline)?;
        self.check_simulated_error()?;
        let handle = format!("fake-handle-{}", uuid::Uuid::new_v4());
        let reference = SecretRef::new(handle.clone());
        let mut map = self.storage.write().unwrap();
        map.retain(|(stored_extension, stored_purpose, _), _| {
            stored_extension != extension_id || stored_purpose != purpose
        });
        map.insert(
            (extension_id.clone(), purpose.clone(), handle),
            value.to_vec(),
        );
        Ok(reference)
    }

    fn read(
        &self,
        extension_id: &ExtensionId,
        purpose: &SecretPurpose,
        reference: &SecretRef,
        deadline: Instant,
    ) -> Result<Option<Vec<u8>>, SecretBrokerError> {
        ensure_deadline(deadline)?;
        validate_reference(reference)?;
        self.check_simulated_error()?;
        let map = self.storage.read().unwrap();
        let key = (
            extension_id.clone(),
            purpose.clone(),
            reference.handle.clone(),
        );
        Ok(map.get(&key).cloned())
    }

    fn delete(
        &self,
        extension_id: &ExtensionId,
        purpose: &SecretPurpose,
        reference: &SecretRef,
        deadline: Instant,
    ) -> Result<(), SecretBrokerError> {
        ensure_deadline(deadline)?;
        validate_reference(reference)?;
        self.check_simulated_error()?;
        let mut map = self.storage.write().unwrap();
        let key = (
            extension_id.clone(),
            purpose.clone(),
            reference.handle.clone(),
        );
        map.remove(&key);
        Ok(())
    }

    fn delete_all(
        &self,
        extension_id: &ExtensionId,
        deadline: Instant,
    ) -> Result<(), SecretBrokerError> {
        ensure_deadline(deadline)?;
        self.check_simulated_error()?;
        let mut map = self.storage.write().unwrap();
        map.retain(|(ext_id, _, _), _| ext_id != extension_id);
        Ok(())
    }
}

fn validate_reference(reference: &SecretRef) -> Result<(), SecretBrokerError> {
    if reference.handle.is_empty() || reference.handle.len() > 256 {
        Err(SecretBrokerError::InvalidReference(
            "secret reference is malformed".into(),
        ))
    } else {
        Ok(())
    }
}

/// Production secret broker backed by Freedesktop Secret Service via `oo7`.
pub struct Oo7SecretBroker {
    backend: SharedSecretBackend,
}

impl Oo7SecretBroker {
    /// Constructs a broker without contacting Secret Service.
    pub fn new() -> Self {
        Self::from_initializer(Arc::new(|deadline| {
            let keyring = block_on_oo7(
                async { oo7::Keyring::new().await.map_err(map_oo7_error) },
                deadline,
            )?;
            Ok(Arc::new(Oo7ConnectedBroker { keyring }))
        }))
    }

    fn from_initializer(initializer: BackendInitializer) -> Self {
        Self {
            backend: SharedSecretBackend {
                state: Mutex::new(BackendState::Uninitialized),
                state_changed: Condvar::new(),
                initializer,
            },
        }
    }

    #[cfg(test)]
    fn with_initializer<F>(initializer: F) -> Self
    where
        F: Fn(Instant) -> Result<Arc<dyn SecretBroker>, SecretBrokerError> + Send + Sync + 'static,
    {
        Self::from_initializer(Arc::new(initializer))
    }
}

impl Default for Oo7SecretBroker {
    fn default() -> Self {
        Self::new()
    }
}

type BackendInitializer =
    Arc<dyn Fn(Instant) -> Result<Arc<dyn SecretBroker>, SecretBrokerError> + Send + Sync>;

enum BackendState {
    Uninitialized,
    Initializing,
    Ready(Arc<dyn SecretBroker>),
    Failed(SecretBrokerError),
}

struct SharedSecretBackend {
    state: Mutex<BackendState>,
    state_changed: Condvar,
    initializer: BackendInitializer,
}

impl SharedSecretBackend {
    fn get(&self, deadline: Instant) -> Result<Arc<dyn SecretBroker>, SecretBrokerError> {
        ensure_deadline(deadline)?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            match &*state {
                BackendState::Ready(backend) => return Ok(backend.clone()),
                BackendState::Failed(error) => return Err(error.clone()),
                BackendState::Uninitialized => {
                    *state = BackendState::Initializing;
                    drop(state);
                    let initialized = (self.initializer)(deadline);
                    state = self
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    match initialized {
                        Ok(backend) => {
                            *state = BackendState::Ready(backend.clone());
                            self.state_changed.notify_all();
                            return Ok(backend);
                        }
                        Err(error) => {
                            *state = BackendState::Failed(error.clone());
                            self.state_changed.notify_all();
                            return Err(error);
                        }
                    }
                }
                BackendState::Initializing => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(deadline_expired());
                    }
                    let (next_state, timeout) = self
                        .state_changed
                        .wait_timeout(state, remaining)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state = next_state;
                    if timeout.timed_out() {
                        return Err(deadline_expired());
                    }
                }
            }
        }
    }
}

struct Oo7ConnectedBroker {
    keyring: oo7::Keyring,
}

fn map_oo7_error(error: oo7::Error) -> SecretBrokerError {
    match error {
        oo7::Error::File(oo7::file::Error::Locked) => {
            SecretBrokerError::Locked("secret storage is locked".into())
        }
        oo7::Error::DBus(oo7::dbus::Error::Dismissed) => {
            SecretBrokerError::Denied("secret-service prompt was dismissed".into())
        }
        oo7::Error::DBus(oo7::dbus::Error::Service(oo7::dbus::ServiceError::IsLocked(_))) => {
            SecretBrokerError::Locked("secret storage is locked".into())
        }
        oo7::Error::DBus(oo7::dbus::Error::Service(oo7::dbus::ServiceError::NoSuchObject(_))) => {
            SecretBrokerError::NotFound("secret item no longer exists".into())
        }
        oo7::Error::DBus(oo7::dbus::Error::ZBus(_)) => {
            SecretBrokerError::BackendUnavailable("Secret Service connection failed".into())
        }
        oo7::Error::DBus(oo7::dbus::Error::Service(_))
        | oo7::Error::DBus(oo7::dbus::Error::Deleted)
        | oo7::Error::DBus(oo7::dbus::Error::NotFound(_))
        | oo7::Error::DBus(oo7::dbus::Error::IO(_))
        | oo7::Error::DBus(oo7::dbus::Error::Crypto(_)) => {
            SecretBrokerError::Internal("Secret Service operation failed".into())
        }
        oo7::Error::File(_) => {
            SecretBrokerError::Internal("secret storage operation failed".into())
        }
    }
}

fn ensure_deadline(deadline: Instant) -> Result<(), SecretBrokerError> {
    if Instant::now() >= deadline {
        Err(deadline_expired())
    } else {
        Ok(())
    }
}

fn deadline_expired() -> SecretBrokerError {
    SecretBrokerError::Cancelled("secret operation deadline expired".into())
}

fn block_on_oo7<F, T>(future: F, deadline: Instant) -> Result<T, SecretBrokerError>
where
    F: std::future::Future<Output = Result<T, SecretBrokerError>>,
{
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(deadline_expired());
    }
    smol::block_on(async move {
        futures_lite::future::race(future, async move {
            smol::Timer::after(remaining).await;
            Err(deadline_expired())
        })
        .await
    })
}

impl SecretBroker for Oo7SecretBroker {
    fn set(
        &self,
        extension_id: &ExtensionId,
        purpose: &SecretPurpose,
        value: &[u8],
        deadline: Instant,
    ) -> Result<SecretRef, SecretBrokerError> {
        ensure_deadline(deadline)?;
        self.backend
            .get(deadline)?
            .set(extension_id, purpose, value, deadline)
    }

    fn read(
        &self,
        extension_id: &ExtensionId,
        purpose: &SecretPurpose,
        reference: &SecretRef,
        deadline: Instant,
    ) -> Result<Option<Vec<u8>>, SecretBrokerError> {
        ensure_deadline(deadline)?;
        validate_reference(reference)?;
        self.backend
            .get(deadline)?
            .read(extension_id, purpose, reference, deadline)
    }

    fn delete(
        &self,
        extension_id: &ExtensionId,
        purpose: &SecretPurpose,
        reference: &SecretRef,
        deadline: Instant,
    ) -> Result<(), SecretBrokerError> {
        ensure_deadline(deadline)?;
        validate_reference(reference)?;
        self.backend
            .get(deadline)?
            .delete(extension_id, purpose, reference, deadline)
    }

    fn delete_all(
        &self,
        extension_id: &ExtensionId,
        deadline: Instant,
    ) -> Result<(), SecretBrokerError> {
        ensure_deadline(deadline)?;
        self.backend
            .get(deadline)?
            .delete_all(extension_id, deadline)
    }
}

impl SecretBroker for Oo7ConnectedBroker {
    fn set(
        &self,
        extension_id: &ExtensionId,
        purpose: &SecretPurpose,
        value: &[u8],
        deadline: Instant,
    ) -> Result<SecretRef, SecretBrokerError> {
        let handle = format!("secret-{}", uuid::Uuid::new_v4());
        let label = format!("shilpo:secret:{extension_id}:{purpose}");

        let result = block_on_oo7(
            async {
                let mut attributes = HashMap::new();
                attributes.insert("shilpo:app", "shilpo");
                attributes.insert("shilpo:extension_id", extension_id.as_str());
                attributes.insert("shilpo:purpose", purpose.as_str());
                attributes.insert("shilpo:handle", handle.as_str());

                let previous = self
                    .keyring
                    .search_items(&[
                        ("shilpo:app", "shilpo"),
                        ("shilpo:extension_id", extension_id.as_str()),
                        ("shilpo:purpose", purpose.as_str()),
                    ])
                    .await
                    .map_err(map_oo7_error)?;

                self.keyring
                    .create_item(&label, &attributes, value, true)
                    .await
                    .map_err(map_oo7_error)?;
                for item in previous {
                    item.delete().await.map_err(map_oo7_error)?;
                }
                Ok(())
            },
            deadline,
        );

        match result {
            Ok(_) => Ok(SecretRef::new(handle)),
            Err(err) => Err(err),
        }
    }

    fn read(
        &self,
        extension_id: &ExtensionId,
        purpose: &SecretPurpose,
        reference: &SecretRef,
        deadline: Instant,
    ) -> Result<Option<Vec<u8>>, SecretBrokerError> {
        ensure_deadline(deadline)?;
        validate_reference(reference)?;
        block_on_oo7(
            async {
                let items = self
                    .keyring
                    .search_items(&[
                        ("shilpo:app", "shilpo"),
                        ("shilpo:extension_id", extension_id.as_str()),
                        ("shilpo:purpose", purpose.as_str()),
                        ("shilpo:handle", reference.handle.as_str()),
                    ])
                    .await
                    .map_err(map_oo7_error)?;

                if let Some(item) = items.first() {
                    let secret = item.secret().await.map_err(map_oo7_error)?;
                    Ok(Some(secret.to_vec()))
                } else {
                    Ok(None)
                }
            },
            deadline,
        )
    }

    fn delete(
        &self,
        extension_id: &ExtensionId,
        purpose: &SecretPurpose,
        reference: &SecretRef,
        deadline: Instant,
    ) -> Result<(), SecretBrokerError> {
        ensure_deadline(deadline)?;
        validate_reference(reference)?;
        block_on_oo7(
            async {
                let items = self
                    .keyring
                    .search_items(&[
                        ("shilpo:app", "shilpo"),
                        ("shilpo:extension_id", extension_id.as_str()),
                        ("shilpo:purpose", purpose.as_str()),
                        ("shilpo:handle", reference.handle.as_str()),
                    ])
                    .await
                    .map_err(map_oo7_error)?;

                for item in items {
                    item.delete().await.map_err(map_oo7_error)?;
                }
                Ok(())
            },
            deadline,
        )
    }

    fn delete_all(
        &self,
        extension_id: &ExtensionId,
        deadline: Instant,
    ) -> Result<(), SecretBrokerError> {
        block_on_oo7(
            async {
                let items = self
                    .keyring
                    .search_items(&[
                        ("shilpo:app", "shilpo"),
                        ("shilpo:extension_id", extension_id.as_str()),
                    ])
                    .await
                    .map_err(map_oo7_error)?;

                for item in items {
                    item.delete().await.map_err(map_oo7_error)?;
                }
                Ok(())
            },
            deadline,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    fn deadline() -> Instant {
        Instant::now() + std::time::Duration::from_secs(5)
    }

    pub fn assert_secret_broker_contract(broker: &dyn SecretBroker) {
        let ext_a = ExtensionId::new("io.github.alice.secrets").unwrap();
        let ext_b = ExtensionId::new("io.github.bob.secrets").unwrap();
        let purpose_1 = SecretPurpose::parse("api-token").unwrap();
        let purpose_2 = SecretPurpose::parse("refresh-token").unwrap();

        // 1. set and read
        let ref_a1 = broker
            .set(&ext_a, &purpose_1, b"super-secret-1", deadline())
            .unwrap();
        let read_a1 = broker
            .read(&ext_a, &purpose_1, &ref_a1, deadline())
            .unwrap();
        assert_eq!(read_a1.as_deref(), Some(&b"super-secret-1"[..]));

        // 2. replace
        let ref_a1_new = broker
            .set(&ext_a, &purpose_1, b"super-secret-1-updated", deadline())
            .unwrap();
        let read_a1_new = broker
            .read(&ext_a, &purpose_1, &ref_a1_new, deadline())
            .unwrap();
        assert_eq!(read_a1_new.as_deref(), Some(&b"super-secret-1-updated"[..]));

        // 3. unique opaque handles
        assert_ne!(ref_a1.handle, ref_a1_new.handle);

        // 4. cross-purpose isolation
        let read_wrong_purpose = broker
            .read(&ext_a, &purpose_2, &ref_a1_new, deadline())
            .unwrap();
        assert_eq!(read_wrong_purpose, None);

        // 5. cross-extension isolation
        let read_wrong_ext = broker
            .read(&ext_b, &purpose_1, &ref_a1_new, deadline())
            .unwrap();
        assert_eq!(read_wrong_ext, None);

        // 6. delete and missing read
        broker
            .delete(&ext_a, &purpose_1, &ref_a1_new, deadline())
            .unwrap();
        let read_deleted = broker
            .read(&ext_a, &purpose_1, &ref_a1_new, deadline())
            .unwrap();
        assert_eq!(read_deleted, None);

        // 7. idempotent delete
        assert!(
            broker
                .delete(&ext_a, &purpose_1, &ref_a1_new, deadline())
                .is_ok()
        );

        // 8. delete_all
        let ref_b1 = broker
            .set(&ext_b, &purpose_1, b"bob-secret", deadline())
            .unwrap();
        let ref_b2 = broker
            .set(&ext_b, &purpose_2, b"bob-refresh", deadline())
            .unwrap();
        assert!(
            broker
                .read(&ext_b, &purpose_1, &ref_b1, deadline())
                .unwrap()
                .is_some()
        );
        assert!(
            broker
                .read(&ext_b, &purpose_2, &ref_b2, deadline())
                .unwrap()
                .is_some()
        );

        broker.delete_all(&ext_b, deadline()).unwrap();
        assert_eq!(
            broker
                .read(&ext_b, &purpose_1, &ref_b1, deadline())
                .unwrap(),
            None
        );
        assert_eq!(
            broker
                .read(&ext_b, &purpose_2, &ref_b2, deadline())
                .unwrap(),
            None
        );
    }

    #[test]
    fn fake_secret_broker_passes_contract_suite() {
        let fake = FakeSecretBroker::new();
        assert_secret_broker_contract(&fake);
    }

    #[test]
    fn fake_secret_broker_simulates_typed_error_outcomes() {
        let fake = FakeSecretBroker::new();
        let ext = ExtensionId::new("io.github.test.errors").unwrap();
        let purpose = SecretPurpose::parse("api-key").unwrap();

        fake.set_simulated_error(Some(SecretBrokerError::BackendUnavailable(
            "daemon down".into(),
        )));
        assert!(matches!(
            fake.set(&ext, &purpose, b"val", deadline()),
            Err(SecretBrokerError::BackendUnavailable(_))
        ));

        fake.set_simulated_error(Some(SecretBrokerError::Locked("keyring locked".into())));
        assert!(matches!(
            fake.read(&ext, &purpose, &SecretRef::new("h1"), deadline()),
            Err(SecretBrokerError::Locked(_))
        ));

        fake.set_simulated_error(Some(SecretBrokerError::Denied("user denied".into())));
        assert!(matches!(
            fake.delete(&ext, &purpose, &SecretRef::new("h1"), deadline()),
            Err(SecretBrokerError::Denied(_))
        ));

        fake.set_simulated_error(Some(SecretBrokerError::Cancelled("timeout".into())));
        assert!(matches!(
            fake.delete_all(&ext, deadline()),
            Err(SecretBrokerError::Cancelled(_))
        ));

        fake.set_simulated_error(Some(SecretBrokerError::NotFound("missing".into())));
        assert!(matches!(
            fake.read(&ext, &purpose, &SecretRef::new("h1"), deadline()),
            Err(SecretBrokerError::NotFound(_))
        ));

        fake.set_simulated_error(Some(SecretBrokerError::Internal("broken".into())));
        assert!(matches!(
            fake.set(&ext, &purpose, b"val", deadline()),
            Err(SecretBrokerError::Internal(_))
        ));
        fake.set_simulated_error(None);
        assert!(matches!(
            fake.read(&ext, &purpose, &SecretRef::new(""), deadline()),
            Err(SecretBrokerError::InvalidReference(_))
        ));
        assert!(matches!(
            fake.read(
                &ext,
                &purpose,
                &SecretRef::new("h1"),
                Instant::now() - std::time::Duration::from_secs(1),
            ),
            Err(SecretBrokerError::Cancelled(_))
        ));
    }

    #[test]
    fn concurrent_first_operations_share_one_backend_initialization_attempt() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_for_init = attempts.clone();
        let broker = Arc::new(Oo7SecretBroker::with_initializer(move |_| {
            attempts_for_init.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(50));
            Err(SecretBrokerError::BackendUnavailable(
                "Secret Service connection failed".into(),
            ))
        }));
        let barrier = Arc::new(Barrier::new(8));

        let callers = (0..8)
            .map(|_| {
                let broker = broker.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    broker.delete_all(
                        &ExtensionId::new("io.github.test.concurrent").unwrap(),
                        deadline(),
                    )
                })
            })
            .collect::<Vec<_>>();

        for caller in callers {
            assert_eq!(
                caller.join().unwrap(),
                Err(SecretBrokerError::BackendUnavailable(
                    "Secret Service connection failed".into()
                ))
            );
        }
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn lazily_initialized_backend_preserves_secret_contract() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_for_init = attempts.clone();
        let broker = Oo7SecretBroker::with_initializer(move |_| {
            attempts_for_init.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(FakeSecretBroker::new()))
        });

        assert_secret_broker_contract(&broker);
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn real_oo7_secret_broker_contract_or_skipped() {
        let broker = Oo7SecretBroker::new();
        let ext = ExtensionId::new("io.github.test.oo7check").unwrap();
        let purpose = SecretPurpose::parse("check").unwrap();
        match broker.set(&ext, &purpose, b"test-probe", deadline()) {
            Ok(reference) => {
                let _ = broker.delete(&ext, &purpose, &reference, deadline());
                let _ = broker.delete_all(&ext, deadline());
                assert_secret_broker_contract(&broker);
            }
            Err(SecretBrokerError::BackendUnavailable(msg)) => {
                println!(
                    "SKIPPED real-keyring integration test: Secret Service unavailable on DBus ({msg})"
                );
            }
            Err(err) => {
                println!("SKIPPED real-keyring integration test: {err}");
            }
        }
    }
}
