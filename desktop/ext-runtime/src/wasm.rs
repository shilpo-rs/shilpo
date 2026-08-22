use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::thread;
use std::time::Duration;
use std::time::Instant;

use shilpo_ext_api::{
    Capability, ExtensionEvent as ApiEvent, ExtensionId, HostOperation, ViewTree as ApiViewTree,
    wildcard_matches,
};
use wasmtime::component::{Component, Linker, ResourceTable, types::ComponentItem};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder, Trap};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::adapter::{
    ExtensionRuntime, GrantChecker, RuntimeBudget, RuntimeError, RuntimeFailureKind,
};
use crate::state::{
    MAX_PENDING_STATE_EVENTS, StateStore, StateStoreError, StateValue as StoredValue,
};

wasmtime::component::bindgen!({
    path: "../../core/ext-api/wit",
    world: "extension",
});

const EPOCH_TICK: Duration = Duration::from_millis(5);

#[derive(Clone, Debug)]
pub struct WasmModule {
    bytes: Arc<[u8]>,
}

impl WasmModule {
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            bytes: bytes.into().into(),
        }
    }

    pub fn from_file(path: &Path) -> Result<Self, RuntimeError> {
        fs::read(path).map(Self::from_bytes).map_err(|error| {
            RuntimeError::with_kind(
                RuntimeFailureKind::Load,
                format!("failed to read WASM component {}: {error}", path.display()),
            )
        })
    }
}

pub struct WasmState {
    pub extension_id: ExtensionId,
    pub declared_capabilities: Vec<Capability>,
    pub granted_capabilities: Vec<Capability>,
    pub secret_broker: Arc<dyn crate::secrets::SecretBroker>,
    pub grant_checker: Option<GrantChecker>,
    pub limits: StoreLimits,
    pub table: ResourceTable,
    pub wasi: WasiCtx,
    pub operations: Vec<HostOperation>,
    pub state_store: Arc<dyn StateStore>,
    pub watch_registry: Arc<Mutex<HashMap<ExtensionId, HashMap<u64, WatchEntry>>>>,
    pub pending_state_events: Arc<Mutex<HashMap<ExtensionId, VecDeque<PendingStateEvent>>>>,
    pub next_watch_id: Arc<AtomicU64>,
    pub state_operation_lock: Arc<Mutex<()>>,
    pub hostcall_bytes: usize,
    pub max_hostcall_bytes: usize,
    pub secret_deadline: Instant,
}

/// A live watch registration held by the runtime on behalf of a guest instance.
#[derive(Clone, Debug)]
pub struct WatchEntry {
    key: String,
}

/// A state-change event queued for delivery to a guest after the initiating host
/// call returns. Values are never logged or exported from the worker process.
#[derive(Clone, Debug)]
pub struct PendingStateEvent {
    watch_id: u64,
    key: String,
    value: Option<serde_json::Value>,
    revision: u64,
}

impl WasiView for WasmState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}
impl wasmtime::component::HasData for WasmState {
    type Data<'a> = &'a mut WasmState;
}

fn get_wasm_state(state: &mut WasmState) -> &mut WasmState {
    state
}

impl WasmState {
    fn check_capability(&self, predicate: impl Fn(&Capability) -> bool) -> bool {
        self.declared_capabilities.iter().any(&predicate)
            && self.granted_capabilities.iter().any(&predicate)
    }

    fn charge_hostcall_bytes(
        &mut self,
        bytes: usize,
    ) -> Result<(), shilpo::extension::types::Error> {
        self.hostcall_bytes += bytes;
        if self.hostcall_bytes > self.max_hostcall_bytes {
            return Err(shilpo::extension::types::Error {
                kind: shilpo::extension::types::ErrorKind::RateLimited,
                message: "host call byte limit exceeded".into(),
            });
        }
        Ok(())
    }
}

fn unauthorized_error(msg: impl Into<String>) -> shilpo::extension::types::Error {
    shilpo::extension::types::Error {
        kind: shilpo::extension::types::ErrorKind::Unauthorized,
        message: msg.into(),
    }
}

impl shilpo::extension::actions::Host for WasmState {
    fn invoke(
        &mut self,
        action_id: String,
        payload: Option<shilpo::extension::types::DataValue>,
    ) -> Result<(), shilpo::extension::types::Error> {
        let payload_json = match payload.as_ref() {
            Some(shilpo::extension::types::DataValue::SecretRef(_)) => {
                return Err(shilpo::extension::types::Error {
                    kind: shilpo::extension::types::ErrorKind::InvalidArgument,
                    message: "secret references cannot cross the action effect boundary".into(),
                });
            }
            Some(value) => Some(data_value_to_json(value).to_string()),
            None => None,
        };
        let call_bytes = action_id.len() + payload_json.as_ref().map_or(0, String::len);
        self.charge_hostcall_bytes(call_bytes)?;
        let allowed = self.check_capability(|cap| match cap {
            Capability::ActionsInvoke { actions } => actions
                .iter()
                .any(|pattern| wildcard_matches(pattern, &action_id)),
            _ => false,
        });
        if !allowed {
            return Err(unauthorized_error("actions:invoke capability denied"));
        }
        self.operations.push(HostOperation::InvokeAction {
            action_id,
            payload_json,
        });
        Ok(())
    }
}

impl shilpo::extension::clipboard::Host for WasmState {
    fn read(&mut self) -> Result<String, shilpo::extension::types::Error> {
        self.charge_hostcall_bytes(0)?;
        let allowed = self.check_capability(|cap| matches!(cap, Capability::ClipboardRead));
        if !allowed {
            return Err(unauthorized_error("clipboard:read capability denied"));
        }
        Ok(String::new())
    }

    fn write(&mut self, text: String) -> Result<(), shilpo::extension::types::Error> {
        self.charge_hostcall_bytes(text.len())?;
        let allowed = self.check_capability(|cap| matches!(cap, Capability::ClipboardWrite));
        if !allowed {
            return Err(unauthorized_error("clipboard:write capability denied"));
        }
        self.operations.push(HostOperation::ClipboardWrite { text });
        Ok(())
    }
}

impl shilpo::extension::filesystem::Host for WasmState {
    fn read_file(&mut self, path: String) -> Result<Vec<u8>, shilpo::extension::types::Error> {
        self.charge_hostcall_bytes(path.len())?;
        let allowed = self.check_capability(|cap| match cap {
            Capability::FilesystemRead { paths } => {
                paths.iter().any(|pattern| wildcard_matches(pattern, &path))
            }
            _ => false,
        });
        if !allowed {
            return Err(unauthorized_error("filesystem:read capability denied"));
        }
        Err(shilpo::extension::types::Error {
            kind: shilpo::extension::types::ErrorKind::NotFound,
            message: format!("virtual file '{path}' not found"),
        })
    }

    fn write_file(
        &mut self,
        path: String,
        contents: Vec<u8>,
    ) -> Result<(), shilpo::extension::types::Error> {
        self.charge_hostcall_bytes(path.len() + contents.len())?;
        let allowed = self.check_capability(|cap| match cap {
            Capability::FilesystemWrite { paths } => {
                paths.iter().any(|pattern| wildcard_matches(pattern, &path))
            }
            _ => false,
        });
        if !allowed {
            return Err(unauthorized_error("filesystem:write capability denied"));
        }
        Ok(())
    }
}

impl shilpo::extension::http::Host for WasmState {
    fn request(
        &mut self,
        req: shilpo::extension::http::HttpRequest,
    ) -> Result<String, shilpo::extension::types::Error> {
        let call_bytes =
            req.url.len() + req.method.len() + req.body.as_ref().map_or(0, |b| b.len());
        self.charge_hostcall_bytes(call_bytes)?;
        let target =
            crate::effects::CanonicalHttpTarget::parse(&req.url, &req.method).ok_or_else(|| {
                shilpo::extension::types::Error {
                    kind: shilpo::extension::types::ErrorKind::InvalidArgument,
                    message: "invalid HTTP request URL or method".into(),
                }
            })?;
        let allowed = self
            .check_capability(|cap| crate::effects::capability_allows_http_target(cap, &target));
        if !allowed {
            return Err(unauthorized_error("network:http capability denied"));
        }
        let req_id = req.request_id.clone();
        self.operations.push(HostOperation::HttpRequest {
            request_id: req.request_id,
            url: req.url,
            method: req.method,
        });
        Ok(req_id)
    }

    fn cancel(&mut self, _req_id: String) -> Result<(), shilpo::extension::types::Error> {
        self.charge_hostcall_bytes(0)?;
        Ok(())
    }
}

impl shilpo::extension::types::Host for WasmState {}
impl shilpo::extension::events::Host for WasmState {}
impl shilpo::extension::view::Host for WasmState {}

impl shilpo::extension::location::Host for WasmState {
    fn read(&mut self) -> Result<String, shilpo::extension::types::Error> {
        self.charge_hostcall_bytes(0)?;
        let allowed = self.check_capability(|cap| matches!(cap, Capability::LocationRead));
        if !allowed {
            return Err(unauthorized_error("location:read capability denied"));
        }
        let req_id = format!("loc-{}", self.operations.len() + 1);
        self.operations.push(HostOperation::LocationRead);
        Ok(req_id)
    }
}

impl shilpo::extension::notifications::Host for WasmState {
    fn show(
        &mut self,
        req: shilpo::extension::notifications::NotificationRequest,
    ) -> Result<(), shilpo::extension::types::Error> {
        let call_bytes =
            req.title.len() + req.body.len() + req.icon.as_ref().map_or(0, |i| i.len());
        self.charge_hostcall_bytes(call_bytes)?;
        let allowed = self.check_capability(|cap| matches!(cap, Capability::NotificationsShow));
        if !allowed {
            return Err(unauthorized_error("notifications:show capability denied"));
        }
        self.operations.push(HostOperation::ShowNotification {
            title: req.title,
            body: req.body,
            icon: req.icon,
        });
        Ok(())
    }
}

impl shilpo::extension::state::Host for WasmState {
    fn read(
        &mut self,
        key: String,
    ) -> Result<self::shilpo::extension::state::StateSnapshot, shilpo::extension::types::Error>
    {
        let operation_lock = self.state_operation_lock.clone();
        let _operation = operation_lock
            .lock()
            .expect("state operation lock poisoned");
        self.charge_hostcall_bytes(key.len())?;
        let snapshot = self
            .state_store
            .read(&self.extension_id, &key)
            .map_err(map_state_error)?;
        Ok(self::shilpo::extension::state::StateSnapshot {
            value: snapshot.value.as_ref().map(data_value_from_stored),
            revision: snapshot.revision,
        })
    }

    fn write(
        &mut self,
        key: String,
        value: shilpo::extension::types::DataValue,
    ) -> Result<self::shilpo::extension::state::StateMutation, shilpo::extension::types::Error>
    {
        let operation_lock = self.state_operation_lock.clone();
        let _operation = operation_lock
            .lock()
            .expect("state operation lock poisoned");
        if matches!(value, shilpo::extension::types::DataValue::SecretRef(_)) {
            return Err(shilpo::extension::types::Error {
                kind: shilpo::extension::types::ErrorKind::InvalidArgument,
                message: "secret references cannot be stored in extension state".into(),
            });
        }
        let stored = stored_value_from_data_value(&value)?;
        let encoded_len = crate::state::encoded_len(&stored);
        self.charge_hostcall_bytes(key.len() + encoded_len)?;
        let mutation = self
            .state_store
            .write(&self.extension_id, &key, stored.clone())
            .map_err(map_state_error)?;
        if mutation.changed {
            self.queue_state_events(&key, Some(stored), mutation.revision);
        }
        Ok(self::shilpo::extension::state::StateMutation {
            changed: mutation.changed,
            revision: mutation.revision,
        })
    }

    fn delete(
        &mut self,
        key: String,
    ) -> Result<self::shilpo::extension::state::StateMutation, shilpo::extension::types::Error>
    {
        let operation_lock = self.state_operation_lock.clone();
        let _operation = operation_lock
            .lock()
            .expect("state operation lock poisoned");
        self.charge_hostcall_bytes(key.len())?;
        let mutation = self
            .state_store
            .delete(&self.extension_id, &key)
            .map_err(map_state_error)?;
        if mutation.changed {
            self.queue_state_events(&key, None, mutation.revision);
        }
        Ok(self::shilpo::extension::state::StateMutation {
            changed: mutation.changed,
            revision: mutation.revision,
        })
    }

    fn watch(
        &mut self,
        key: String,
    ) -> Result<self::shilpo::extension::state::WatchRegistration, shilpo::extension::types::Error>
    {
        let operation_lock = self.state_operation_lock.clone();
        let _operation = operation_lock
            .lock()
            .expect("state operation lock poisoned");
        self.charge_hostcall_bytes(key.len())?;
        let snapshot = self
            .state_store
            .read(&self.extension_id, &key)
            .map_err(map_state_error)?;
        let watch_id = self.next_watch_id.fetch_add(1, Ordering::Relaxed);
        self.watch_registry
            .lock()
            .expect("watch registry poisoned")
            .entry(self.extension_id.clone())
            .or_default()
            .insert(watch_id, WatchEntry { key: key.clone() });
        Ok(self::shilpo::extension::state::WatchRegistration {
            watch_id,
            snapshot: self::shilpo::extension::state::StateSnapshot {
                value: snapshot.value.as_ref().map(data_value_from_stored),
                revision: snapshot.revision,
            },
        })
    }

    fn unwatch(&mut self, watch_id: u64) -> Result<(), shilpo::extension::types::Error> {
        let operation_lock = self.state_operation_lock.clone();
        let _operation = operation_lock
            .lock()
            .expect("state operation lock poisoned");
        self.charge_hostcall_bytes(8)?;
        self.watch_registry
            .lock()
            .expect("watch registry poisoned")
            .get_mut(&self.extension_id)
            .map(|registrations| registrations.remove(&watch_id));
        Ok(())
    }
}

impl WasmState {
    /// Queues one state-change event per live watch on `key`, coalescing to the
    /// latest revision per watch when the pending queue exceeds its bound.
    fn queue_state_events(&mut self, key: &str, value: Option<StoredValue>, revision: u64) {
        let registrations: Vec<u64> = self
            .watch_registry
            .lock()
            .expect("watch registry poisoned")
            .get(&self.extension_id)
            .map(|registrations| {
                registrations
                    .iter()
                    .filter(|(_, entry)| entry.key == key)
                    .map(|(watch_id, _)| *watch_id)
                    .collect()
            })
            .unwrap_or_default();
        if registrations.is_empty() {
            return;
        }
        let value_json = value.as_ref().map(stored_value_to_json);
        let mut pending = self
            .pending_state_events
            .lock()
            .expect("pending state events poisoned");
        let queue = pending.entry(self.extension_id.clone()).or_default();
        for watch_id in registrations {
            queue.push_back(PendingStateEvent {
                watch_id,
                key: key.to_owned(),
                value: value_json.clone(),
                revision,
            });
        }
        while queue.len() > MAX_PENDING_STATE_EVENTS {
            let watch_ids: Vec<u64> = queue.iter().map(|event| event.watch_id).collect();
            let mut coalesced = false;
            for (index, watch_id) in watch_ids.iter().enumerate() {
                if watch_ids[index + 1..].contains(watch_id) {
                    queue.remove(index);
                    coalesced = true;
                    break;
                }
            }
            if !coalesced {
                queue.pop_front();
            }
        }
    }
}

impl WasmState {
    fn check_secret_permission(&self, purpose: &shilpo_ext_api::SecretPurpose) -> bool {
        let declared = self.declared_capabilities.iter().any(
            |cap| matches!(cap, Capability::Secrets { purposes } if purposes.contains(purpose)),
        );
        if !declared {
            return false;
        }
        let scope = format!("secrets:{purpose}");
        if let Some(checker) = &self.grant_checker {
            checker(&self.extension_id, &scope)
        } else {
            self.granted_capabilities.iter().any(
                |cap| matches!(cap, Capability::Secrets { purposes } if purposes.contains(purpose)),
            )
        }
    }
}

fn map_secret_broker_error(
    err: crate::secrets::SecretBrokerError,
) -> shilpo::extension::types::Error {
    use shilpo::extension::types as wit_types;

    use crate::secrets::SecretBrokerError;

    match err {
        SecretBrokerError::BackendUnavailable(msg) => wit_types::Error {
            kind: wit_types::ErrorKind::BackendUnavailable,
            message: msg,
        },
        SecretBrokerError::Locked(msg) => wit_types::Error {
            kind: wit_types::ErrorKind::Locked,
            message: msg,
        },
        SecretBrokerError::Denied(msg) => wit_types::Error {
            kind: wit_types::ErrorKind::Denied,
            message: msg,
        },
        SecretBrokerError::Cancelled(msg) => wit_types::Error {
            kind: wit_types::ErrorKind::Cancelled,
            message: msg,
        },
        SecretBrokerError::NotFound(msg) | SecretBrokerError::InvalidReference(msg) => {
            wit_types::Error {
                kind: wit_types::ErrorKind::NotFound,
                message: msg,
            }
        }
        SecretBrokerError::Internal(msg) => wit_types::Error {
            kind: wit_types::ErrorKind::Internal,
            message: msg,
        },
    }
}

impl shilpo::extension::secrets::Host for WasmState {
    fn set(
        &mut self,
        purpose: String,
        value: Vec<u8>,
    ) -> Result<shilpo::extension::secrets::SecretRef, shilpo::extension::types::Error> {
        self.charge_hostcall_bytes(purpose.len() + value.len())?;
        let parsed_purpose = shilpo_ext_api::SecretPurpose::parse(&purpose).map_err(|err| {
            shilpo::extension::types::Error {
                kind: shilpo::extension::types::ErrorKind::InvalidArgument,
                message: err.to_string(),
            }
        })?;

        if !self.check_secret_permission(&parsed_purpose) {
            return Err(shilpo::extension::types::Error {
                kind: shilpo::extension::types::ErrorKind::Unauthorized,
                message: format!(
                    "extension '{}' missing declaration or grant for secret purpose '{}'",
                    self.extension_id, parsed_purpose
                ),
            });
        }

        let reference = self
            .secret_broker
            .set(
                &self.extension_id,
                &parsed_purpose,
                &value,
                self.secret_deadline,
            )
            .map_err(map_secret_broker_error)?;

        Ok(shilpo::extension::secrets::SecretRef {
            handle: reference.handle,
        })
    }

    fn read(
        &mut self,
        purpose: String,
        reference: shilpo::extension::secrets::SecretRef,
    ) -> Result<Option<Vec<u8>>, shilpo::extension::types::Error> {
        self.charge_hostcall_bytes(purpose.len() + reference.handle.len())?;
        let parsed_purpose = shilpo_ext_api::SecretPurpose::parse(&purpose).map_err(|err| {
            shilpo::extension::types::Error {
                kind: shilpo::extension::types::ErrorKind::InvalidArgument,
                message: err.to_string(),
            }
        })?;

        if !self.check_secret_permission(&parsed_purpose) {
            return Err(shilpo::extension::types::Error {
                kind: shilpo::extension::types::ErrorKind::Unauthorized,
                message: format!(
                    "extension '{}' missing declaration or grant for secret purpose '{}'",
                    self.extension_id, parsed_purpose
                ),
            });
        }

        let api_ref = shilpo_ext_api::SecretRef::new(reference.handle);
        let bytes = self
            .secret_broker
            .read(
                &self.extension_id,
                &parsed_purpose,
                &api_ref,
                self.secret_deadline,
            )
            .map_err(map_secret_broker_error)?;

        Ok(bytes)
    }

    fn delete(
        &mut self,
        purpose: String,
        reference: shilpo::extension::secrets::SecretRef,
    ) -> Result<(), shilpo::extension::types::Error> {
        self.charge_hostcall_bytes(purpose.len() + reference.handle.len())?;
        let parsed_purpose = shilpo_ext_api::SecretPurpose::parse(&purpose).map_err(|err| {
            shilpo::extension::types::Error {
                kind: shilpo::extension::types::ErrorKind::InvalidArgument,
                message: err.to_string(),
            }
        })?;

        if !self.check_secret_permission(&parsed_purpose) {
            return Err(shilpo::extension::types::Error {
                kind: shilpo::extension::types::ErrorKind::Unauthorized,
                message: format!(
                    "extension '{}' missing declaration or grant for secret purpose '{}'",
                    self.extension_id, parsed_purpose
                ),
            });
        }

        let api_ref = shilpo_ext_api::SecretRef::new(reference.handle);
        self.secret_broker
            .delete(
                &self.extension_id,
                &parsed_purpose,
                &api_ref,
                self.secret_deadline,
            )
            .map_err(map_secret_broker_error)?;

        Ok(())
    }
}

impl shilpo::extension::theme::Host for WasmState {
    fn read(
        &mut self,
    ) -> Result<shilpo::extension::theme::ThemeInfo, shilpo::extension::types::Error> {
        self.charge_hostcall_bytes(0)?;
        let allowed = self.check_capability(|cap| matches!(cap, Capability::ThemeRead));
        if !allowed {
            return Err(unauthorized_error("theme:read capability denied"));
        }
        Ok(shilpo::extension::theme::ThemeInfo {
            mode: "dark".into(),
            accent: "#006c4c".into(),
        })
    }

    fn set_source_color(&mut self, color: String) -> Result<(), shilpo::extension::types::Error> {
        self.charge_hostcall_bytes(color.len())?;
        let allowed = self.check_capability(|cap| matches!(cap, Capability::ThemeSetSource));
        if !allowed {
            return Err(unauthorized_error("theme:set_source capability denied"));
        }
        self.operations
            .push(HostOperation::SetThemeSource { color });
        Ok(())
    }
}

impl shilpo::extension::wallpaper::Host for WasmState {
    fn read(
        &mut self,
    ) -> Result<shilpo::extension::wallpaper::WallpaperInfo, shilpo::extension::types::Error> {
        self.charge_hostcall_bytes(0)?;
        let allowed = self.check_capability(|cap| matches!(cap, Capability::WallpaperRead));
        if !allowed {
            return Err(unauthorized_error("wallpaper:read capability denied"));
        }
        Ok(shilpo::extension::wallpaper::WallpaperInfo {
            path: String::new(),
        })
    }

    fn set(
        &mut self,
        req: shilpo::extension::wallpaper::WallpaperSetRequest,
    ) -> Result<(), shilpo::extension::types::Error> {
        self.charge_hostcall_bytes(req.path.len())?;
        let api_source = match req.source {
            shilpo::extension::wallpaper::WallpaperSource::ExtensionAsset => {
                shilpo_ext_api::WallpaperSource::ExtensionAsset
            }
            shilpo::extension::wallpaper::WallpaperSource::LocalFile => {
                shilpo_ext_api::WallpaperSource::LocalFile
            }
            shilpo::extension::wallpaper::WallpaperSource::Remote => {
                shilpo_ext_api::WallpaperSource::Remote
            }
        };
        let allowed = self.check_capability(|cap| match cap {
            Capability::WallpaperSet { sources } => sources.contains(&api_source),
            _ => false,
        });
        if !allowed {
            return Err(unauthorized_error("wallpaper:set capability denied"));
        }
        match api_source {
            shilpo_ext_api::WallpaperSource::ExtensionAsset => {
                let p = Path::new(&req.path);
                if p.is_absolute()
                    || req.path.contains("..")
                    || p.components()
                        .any(|c| matches!(c, std::path::Component::ParentDir))
                {
                    return Err(shilpo::extension::types::Error {
                        kind: shilpo::extension::types::ErrorKind::InvalidArgument,
                        message: "extension_asset path must be relative without traversal".into(),
                    });
                }
            }
            shilpo_ext_api::WallpaperSource::LocalFile => {
                // The Shell validates and expands local paths immediately before
                // handing them to the theme daemon. The runtime must not reject
                // valid host paths such as `~/Pictures/...` in the guest adapter.
            }
            shilpo_ext_api::WallpaperSource::Remote => {
                return Err(shilpo::extension::types::Error {
                    kind: shilpo::extension::types::ErrorKind::Unsupported,
                    message: "remote wallpaper source is not supported without a host-owned bounded downloader".into(),
                });
            }
        }
        let api_target = req.target.map(|t| match t {
            shilpo::extension::wallpaper::WallpaperTarget::Global => {
                shilpo_ext_api::WallpaperTarget::Global
            }
            shilpo::extension::wallpaper::WallpaperTarget::Workspace(w) => {
                shilpo_ext_api::WallpaperTarget::Workspace(shilpo_ext_api::WorkspaceTarget {
                    workspace_id: w.workspace_id,
                    output_name: w.output_name,
                })
            }
        });
        self.operations.push(HostOperation::SetWallpaper {
            path: req.path,
            source: api_source,
            request_id: req.request_id,
            target: api_target,
        });
        Ok(())
    }
}

struct WasmInstance {
    store: Store<WasmState>,
    extension: Extension,
}

pub struct WasmRuntime {
    engine: Engine,
    instances: HashMap<ExtensionId, WasmInstance>,
    pub secret_broker: Arc<dyn crate::secrets::SecretBroker>,
    pub grant_checker: Option<GrantChecker>,
    state_store: Arc<dyn StateStore>,
    watch_registry: Arc<Mutex<HashMap<ExtensionId, HashMap<u64, WatchEntry>>>>,
    pending_state_events: Arc<Mutex<HashMap<ExtensionId, VecDeque<PendingStateEvent>>>>,
    next_watch_id: Arc<AtomicU64>,
    state_operation_lock: Arc<Mutex<()>>,
    ticker_stop: Arc<AtomicBool>,
    ticker: Option<thread::JoinHandle<()>>,
}

impl WasmRuntime {
    pub fn new() -> Result<Self, RuntimeError> {
        Self::new_with_paths(&crate::catalog::CatalogPaths::platform_default())
    }

    pub fn new_with_paths(paths: &crate::catalog::CatalogPaths) -> Result<Self, RuntimeError> {
        let broker: Arc<dyn crate::secrets::SecretBroker> =
            Arc::new(crate::secrets::Oo7SecretBroker::new().map_err(|error| {
                RuntimeError::with_kind(
                    RuntimeFailureKind::Unavailable,
                    format!("failed to initialize Secret Service: {error}"),
                )
            })?);
        let state_store =
            crate::state::HeedStateStore::open(&paths.state_store_dir()).map_err(|error| {
                RuntimeError::with_kind(
                    RuntimeFailureKind::Unavailable,
                    format!("failed to open extension state store: {error}"),
                )
            })?;
        Self::with_broker_and_state_store(broker, Arc::new(state_store))
    }

    pub fn with_broker(
        secret_broker: Arc<dyn crate::secrets::SecretBroker>,
    ) -> Result<Self, RuntimeError> {
        Self::with_broker_and_grant_checker(secret_broker, None)
    }

    pub fn with_broker_and_grant_checker(
        secret_broker: Arc<dyn crate::secrets::SecretBroker>,
        grant_checker: Option<GrantChecker>,
    ) -> Result<Self, RuntimeError> {
        Self::with_broker_and_grant_checker_and_state_store(
            secret_broker,
            grant_checker,
            Arc::new(crate::state::FakeStateStore::default()),
        )
    }

    pub fn with_broker_and_state_store(
        secret_broker: Arc<dyn crate::secrets::SecretBroker>,
        state_store: Arc<dyn StateStore>,
    ) -> Result<Self, RuntimeError> {
        Self::with_broker_and_grant_checker_and_state_store(secret_broker, None, state_store)
    }

    pub fn with_broker_and_grant_checker_and_state_store(
        secret_broker: Arc<dyn crate::secrets::SecretBroker>,
        grant_checker: Option<GrantChecker>,
        state_store: Arc<dyn StateStore>,
    ) -> Result<Self, RuntimeError> {
        let engine = configured_engine()?;
        let ticker_stop = Arc::new(AtomicBool::new(false));
        let ticker_engine = engine.clone();
        let ticker_flag = ticker_stop.clone();
        let ticker = thread::Builder::new()
            .name("shilpo-extension-epoch".into())
            .spawn(move || {
                while !ticker_flag.load(Ordering::Relaxed) {
                    thread::sleep(EPOCH_TICK);
                    ticker_engine.increment_epoch();
                }
            })
            .map_err(|error| {
                RuntimeError::with_kind(
                    RuntimeFailureKind::Load,
                    format!("failed to start WASM deadline ticker: {error}"),
                )
            })?;

        Ok(Self {
            engine,
            instances: HashMap::new(),
            secret_broker,
            grant_checker,
            state_store,
            watch_registry: Arc::new(Mutex::new(HashMap::new())),
            pending_state_events: Arc::new(Mutex::new(HashMap::new())),
            next_watch_id: Arc::new(AtomicU64::new(1)),
            state_operation_lock: Arc::new(Mutex::new(())),
            ticker_stop,
            ticker: Some(ticker),
        })
    }

    /// Default bounded validation timeout for component compilation and interface checking (30s).
    pub const DEFAULT_VALIDATION_TIMEOUT: Duration = Duration::from_secs(30);

    /// Maximum component size accepted for validation (32 MiB).
    pub const MAX_VALIDATION_COMPONENT_SIZE: usize = 32 * 1024 * 1024;

    /// Validates a WebAssembly component binary against the canonical
    /// `shilpo:extension@0.1.0` WIT contract within the default bounded timeout (30s).
    pub fn validate_module(bytes: &[u8]) -> Result<(), RuntimeError> {
        Self::validate_module_timeout(bytes, Self::DEFAULT_VALIDATION_TIMEOUT)
    }

    /// Validates a component in the current process. Production callers should use
    /// `validate_module_timeout`, which isolates compilation in a killable helper process.
    pub fn validate_module_unbounded(bytes: &[u8]) -> Result<(), RuntimeError> {
        Self::validate_module_in_process(bytes)
    }

    /// Validates a WebAssembly component binary against the canonical
    /// `shilpo:extension@0.1.0` WIT contract within an explicit bounded duration.
    pub fn validate_module_timeout(bytes: &[u8], timeout: Duration) -> Result<(), RuntimeError> {
        if let Ok(executable) = std::env::current_exe()
            && executable.file_stem().is_some_and(|stem| stem == "shilpo")
        {
            return Self::validate_module_isolated(bytes, timeout, &executable);
        }

        Self::validate_module_in_process_timeout(bytes, timeout)
    }

    fn validate_module_in_process(bytes: &[u8]) -> Result<(), RuntimeError> {
        if bytes.len() > Self::MAX_VALIDATION_COMPONENT_SIZE {
            return Err(RuntimeError::with_kind(
                RuntimeFailureKind::Load,
                format!(
                    "component size ({} bytes) exceeds maximum supported validation limit of {} bytes",
                    bytes.len(),
                    Self::MAX_VALIDATION_COMPONENT_SIZE
                ),
            ));
        }
        let engine = configured_engine()?;
        let component = compile_component(&engine, bytes)?;
        validate_component_type(&engine, &component)
    }

    fn validate_module_in_process_timeout(
        bytes: &[u8],
        timeout: Duration,
    ) -> Result<(), RuntimeError> {
        if bytes.len() > Self::MAX_VALIDATION_COMPONENT_SIZE {
            return Err(RuntimeError::with_kind(
                RuntimeFailureKind::Load,
                format!(
                    "component size ({} bytes) exceeds maximum supported validation limit of {} bytes",
                    bytes.len(),
                    Self::MAX_VALIDATION_COMPONENT_SIZE
                ),
            ));
        }

        let bytes_vec = bytes.to_vec();
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::Builder::new()
            .name("wasm-validator".into())
            .spawn(move || {
                let res = (|| {
                    let engine = configured_engine()?;
                    let component = compile_component(&engine, &bytes_vec)?;
                    validate_component_type(&engine, &component)
                })();
                let _ = tx.send(res);
            });

        match handle {
            Ok(join_handle) => match rx.recv_timeout(timeout) {
                Ok(res) => {
                    let _ = join_handle.join();
                    res
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    #[cfg(test)]
                    return Err(RuntimeError::with_kind(
                        RuntimeFailureKind::Timeout,
                        format!(
                            "component validation timed out after {:.3}s",
                            timeout.as_secs_f64()
                        ),
                    ));

                    #[cfg(not(test))]
                    {
                        // Component compilation is synchronous and cannot be interrupted by
                        // Wasmtime. Join before returning so a timed-out validation never leaves
                        // compiler work running after its caller has gone away.
                        let _ = join_handle.join();
                        Err(RuntimeError::with_kind(
                            RuntimeFailureKind::Timeout,
                            format!(
                                "component validation timed out after {:.3}s",
                                timeout.as_secs_f64()
                            ),
                        ))
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = join_handle.join();
                    Err(RuntimeError::with_kind(
                        RuntimeFailureKind::Load,
                        "component validation thread terminated unexpectedly".to_string(),
                    ))
                }
            },
            Err(error) => Err(RuntimeError::with_kind(
                RuntimeFailureKind::Load,
                format!("failed to spawn validation thread: {error}"),
            )),
        }
    }

    fn validate_module_isolated(
        bytes: &[u8],
        timeout: Duration,
        executable: &Path,
    ) -> Result<(), RuntimeError> {
        let path = std::env::temp_dir().join(format!(
            "shilpo-wasm-validator-{}-{}.wasm",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::write(&path, bytes).map_err(|error| {
            RuntimeError::with_kind(
                RuntimeFailureKind::Load,
                format!("failed to stage component for validation: {error}"),
            )
        })?;
        let mut child = std::process::Command::new(executable)
            .env("SHILPO_WASM_VALIDATOR", &path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| {
                RuntimeError::with_kind(
                    RuntimeFailureKind::Load,
                    format!("failed to start isolated component validator: {error}"),
                )
            })?;
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = child.try_wait().map_err(|error| {
                RuntimeError::with_kind(
                    RuntimeFailureKind::Load,
                    format!("failed to poll isolated component validator: {error}"),
                )
            })? {
                let _ = fs::remove_file(&path);
                if status.success() {
                    return Ok(());
                }
                let mut error_message = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    let _ = std::io::Read::read_to_string(&mut stderr, &mut error_message);
                }
                let error_message = error_message.trim();
                let message = if error_message.is_empty() {
                    "component validation failed in isolated worker".to_string()
                } else {
                    error_message.to_string()
                };
                return Err(RuntimeError::with_kind(RuntimeFailureKind::Load, message));
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                let _ = fs::remove_file(&path);
                return Err(RuntimeError::with_kind(
                    RuntimeFailureKind::Timeout,
                    format!(
                        "component validation timed out after {:.3}s",
                        timeout.as_secs_f64()
                    ),
                ));
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Drops all watch registrations and undelivered state events for an
    /// extension whose instance is being replaced or unloaded.
    fn drop_watch_state(&self, extension_id: &ExtensionId) {
        self.watch_registry
            .lock()
            .expect("watch registry poisoned")
            .remove(extension_id);
        self.pending_state_events
            .lock()
            .expect("pending state events poisoned")
            .remove(extension_id);
    }

    /// Delivers queued state-change events to the guest after the initiating host
    /// call returns, in revision order per watch. A failed delivery stops further
    /// delivery for this batch; values are never logged.
    fn deliver_pending_state_events(
        &mut self,
        extension_id: &ExtensionId,
        ops: &mut Vec<HostOperation>,
    ) {
        let events: Vec<ApiEvent> = {
            let mut pending = self
                .pending_state_events
                .lock()
                .expect("pending state events poisoned");
            pending
                .remove(extension_id)
                .map(|queue| {
                    queue
                        .into_iter()
                        .map(|event| ApiEvent::StateValue {
                            watch_id: event.watch_id,
                            key: event.key,
                            value: event.value,
                            revision: event.revision,
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        for event in events {
            let instance = match self.instances.get_mut(extension_id) {
                Some(instance) => instance,
                None => return,
            };
            let wit_event = convert_event_to_wit(extension_id, &event);
            match instance
                .extension
                .call_on_event(&mut instance.store, &wit_event)
            {
                Ok(Ok(())) => {
                    ops.append(&mut instance.store.data_mut().operations);
                }
                Ok(Err(err)) => {
                    tracing::warn!(
                        target: "shilpo_profile",
                        extension_id = %extension_id,
                        "state event delivery rejected by guest: {}",
                        err.message,
                    );
                    return;
                }
                Err(error) => {
                    tracing::warn!(
                        target: "shilpo_profile",
                        extension_id = %extension_id,
                        "state event delivery failed: {error}",
                    );
                    return;
                }
            }
        }
    }

    fn prepare_call(
        instance: &mut WasmInstance,
        budget: RuntimeBudget,
    ) -> Result<(), RuntimeError> {
        configure_store(&mut instance.store, budget)
    }

    fn instance_mut(
        &mut self,
        extension_id: &ExtensionId,
    ) -> Result<&mut WasmInstance, RuntimeError> {
        self.instances.get_mut(extension_id).ok_or_else(|| {
            RuntimeError::with_kind(
                RuntimeFailureKind::Unavailable,
                format!("extension '{extension_id}' is not loaded"),
            )
        })
    }

    fn instantiate_module(
        &self,
        extension_id: &ExtensionId,
        module: &WasmModule,
        budget: RuntimeBudget,
        declared_capabilities: Vec<Capability>,
        granted_capabilities: Vec<Capability>,
    ) -> Result<WasmInstance, RuntimeError> {
        let component = compile_component(&self.engine, &module.bytes)?;
        validate_component_type(&self.engine, &component)?;
        let mut linker = Linker::<WasmState>::new(&self.engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(|error| {
            RuntimeError::with_kind(
                RuntimeFailureKind::Load,
                format!("failed to configure sandboxed WASI imports: {error:#}"),
            )
        })?;
        Extension::add_to_linker::<WasmState, WasmState>(
            &mut linker,
            get_wasm_state as fn(&mut WasmState) -> &mut WasmState,
        )
        .map_err(|error| {
            RuntimeError::with_kind(
                RuntimeFailureKind::Load,
                format!("failed to configure component imports: {error:#}"),
            )
        })?;
        let state = WasmState {
            extension_id: extension_id.clone(),
            declared_capabilities,
            granted_capabilities,
            secret_broker: self.secret_broker.clone(),
            grant_checker: self.grant_checker.clone(),
            limits: StoreLimitsBuilder::new()
                .memory_size(budget.max_memory_bytes)
                .instances(16)
                .memories(8)
                .tables(16)
                .build(),
            table: ResourceTable::new(),
            wasi: WasiCtxBuilder::new().build(),
            operations: Vec::new(),
            state_store: self.state_store.clone(),
            watch_registry: self.watch_registry.clone(),
            pending_state_events: self.pending_state_events.clone(),
            next_watch_id: self.next_watch_id.clone(),
            state_operation_lock: self.state_operation_lock.clone(),
            hostcall_bytes: 0,
            max_hostcall_bytes: budget.max_hostcall_bytes,
            secret_deadline: Instant::now() + budget.deadline,
        };
        let mut store = Store::new(&self.engine, state);
        store.limiter(|state| &mut state.limits);
        configure_store(&mut store, budget)?;

        let extension = Extension::instantiate(&mut store, &component, &linker)
            .map_err(|error| classify_wasmtime_error("component instantiation failed", error))?;
        Ok(WasmInstance { store, extension })
    }

    pub fn load_with_capabilities(
        &mut self,
        extension_id: &ExtensionId,
        module: WasmModule,
        budget: RuntimeBudget,
        declared_capabilities: Vec<Capability>,
        granted_capabilities: Vec<Capability>,
    ) -> Result<(), RuntimeError> {
        if self.instances.contains_key(extension_id) {
            return Err(RuntimeError::with_kind(
                RuntimeFailureKind::Load,
                format!("extension '{extension_id}' is already loaded"),
            ));
        }
        let instance = self.instantiate_module(
            extension_id,
            &module,
            budget,
            declared_capabilities,
            granted_capabilities,
        )?;
        self.instances.insert(extension_id.clone(), instance);
        Ok(())
    }
}

fn configure_store(
    store: &mut Store<WasmState>,
    budget: RuntimeBudget,
) -> Result<(), RuntimeError> {
    store.set_fuel(budget.fuel).map_err(|error| {
        RuntimeError::with_kind(
            RuntimeFailureKind::Load,
            format!("failed to configure WASM fuel: {error}"),
        )
    })?;
    store.data_mut().hostcall_bytes = 0;
    store.data_mut().max_hostcall_bytes = budget.max_hostcall_bytes;
    store.data_mut().secret_deadline = Instant::now() + budget.deadline;
    let ticks = budget
        .deadline
        .as_nanos()
        .div_ceil(EPOCH_TICK.as_nanos())
        .max(1) as u64;
    store.set_epoch_deadline(ticks);
    Ok(())
}

impl ExtensionRuntime for WasmRuntime {
    type Module = WasmModule;

    fn set_grant_checker(
        &mut self,
        checker: Arc<dyn Fn(&ExtensionId, &str) -> bool + Send + Sync>,
    ) {
        self.grant_checker = Some(checker);
    }

    fn load_with_capabilities(
        &mut self,
        extension_id: &ExtensionId,
        module: Self::Module,
        budget: RuntimeBudget,
        declared_capabilities: Vec<Capability>,
        granted_capabilities: Vec<Capability>,
    ) -> Result<(), RuntimeError> {
        self.load_with_capabilities(
            extension_id,
            module,
            budget,
            declared_capabilities,
            granted_capabilities,
        )
    }

    fn replace_with_capabilities(
        &mut self,
        extension_id: &ExtensionId,
        module: Self::Module,
        budget: RuntimeBudget,
        declared_capabilities: Vec<Capability>,
        granted_capabilities: Vec<Capability>,
    ) -> Result<(), RuntimeError> {
        let replacement = self.instantiate_module(
            extension_id,
            &module,
            budget,
            declared_capabilities,
            granted_capabilities,
        )?;
        self.drop_watch_state(extension_id);
        self.instances.insert(extension_id.clone(), replacement);
        Ok(())
    }

    fn validate_module_with_capabilities(
        &mut self,
        extension_id: &ExtensionId,
        module: &Self::Module,
        budget: RuntimeBudget,
        declared_capabilities: Vec<Capability>,
        granted_capabilities: Vec<Capability>,
    ) -> Result<(), RuntimeError> {
        self.instantiate_module(
            extension_id,
            module,
            budget,
            declared_capabilities,
            granted_capabilities,
        )
        .map(|_| ())
    }

    fn compile_module(&self, bytes: &[u8]) -> Result<Self::Module, String> {
        Ok(WasmModule::from_bytes(bytes.to_vec()))
    }

    fn load(
        &mut self,
        extension_id: &ExtensionId,
        module: Self::Module,
        budget: RuntimeBudget,
    ) -> Result<(), RuntimeError> {
        self.load_with_capabilities(extension_id, module, budget, Vec::new(), Vec::new())
    }

    fn replace(
        &mut self,
        extension_id: &ExtensionId,
        module: Self::Module,
        budget: RuntimeBudget,
    ) -> Result<(), RuntimeError> {
        let span = tracing::info_span!(
            target: "shilpo_profile",
            "extension_wasm_call",
            extension_id = %extension_id,
            operation = "replace",
            fuel = budget.fuel,
            memory_limit = budget.max_memory_bytes,
            outcome = "failure",
        );
        let _enter = span.enter();
        let (declared, granted) = self
            .instances
            .get(extension_id)
            .map(|i| {
                (
                    i.store.data().declared_capabilities.clone(),
                    i.store.data().granted_capabilities.clone(),
                )
            })
            .ok_or_else(|| {
                RuntimeError::with_kind(
                    RuntimeFailureKind::Unavailable,
                    format!("extension '{extension_id}' is not loaded"),
                )
            })?;
        let replacement =
            self.instantiate_module(extension_id, &module, budget, declared, granted)?;
        self.drop_watch_state(extension_id);
        self.instances.insert(extension_id.clone(), replacement);
        span.record("outcome", "success");
        Ok(())
    }

    fn unload(&mut self, extension_id: &ExtensionId) -> Result<(), RuntimeError> {
        self.drop_watch_state(extension_id);
        self.instances
            .remove(extension_id)
            .map(|_| ())
            .ok_or_else(|| {
                RuntimeError::with_kind(
                    RuntimeFailureKind::Unavailable,
                    format!("extension '{extension_id}' is not loaded"),
                )
            })
    }

    fn dispatch(
        &mut self,
        extension_id: &ExtensionId,
        event: &ApiEvent,
        budget: RuntimeBudget,
    ) -> Result<Vec<HostOperation>, RuntimeError> {
        let span = tracing::info_span!(
            target: "shilpo_profile",
            "extension_wasm_call",
            extension_id = %extension_id,
            operation = "dispatch",
            fuel = budget.fuel,
            memory_limit = budget.max_memory_bytes,
            outcome = "failure",
        );
        let _enter = span.enter();
        let instance = self.instance_mut(extension_id)?;
        Self::prepare_call(instance, budget)?;
        instance.store.data_mut().operations.clear();
        let wit_event = convert_event_to_wit(extension_id, event);
        let result = instance
            .extension
            .call_on_event(&mut instance.store, &wit_event)
            .map_err(|error| classify_wasmtime_error("on-event call failed", error))?;
        let mut ops = Vec::new();
        match result {
            Ok(()) => ops.append(&mut instance.store.data_mut().operations),
            Err(err) => {
                self.deliver_pending_state_events(extension_id, &mut ops);
                span.record("outcome", "failure");
                return Err(RuntimeError::with_kind(
                    RuntimeFailureKind::InvalidOutput,
                    format!("on-event failed: {}", err.message),
                ));
            }
        }
        self.deliver_pending_state_events(extension_id, &mut ops);
        span.record("outcome", "success");
        Ok(ops)
    }

    fn view(
        &mut self,
        extension_id: &ExtensionId,
        contribution_id: &str,
        budget: RuntimeBudget,
    ) -> Result<Option<ApiViewTree>, RuntimeError> {
        let span = tracing::info_span!(
            target: "shilpo_profile",
            "extension_wasm_call",
            extension_id = %extension_id,
            operation = "view",
            fuel = budget.fuel,
            memory_limit = budget.max_memory_bytes,
            outcome = "failure",
        );
        let _enter = span.enter();
        let instance = self.instance_mut(extension_id)?;
        Self::prepare_call(instance, budget)?;
        let result = instance
            .extension
            .call_view(&mut instance.store, contribution_id)
            .map_err(|error| classify_wasmtime_error("view call failed", error))?;
        let mut ops = Vec::new();
        let outcome = match result {
            Ok(tree) => {
                span.record("outcome", "success");
                match tree {
                    Some(t) => Some(convert_view_tree_from_wit(t)?),
                    None => None,
                }
            }
            Err(err) => {
                span.record("outcome", "failure");
                return Err(RuntimeError::with_kind(
                    RuntimeFailureKind::InvalidOutput,
                    format!("view failed: {}", err.message),
                ));
            }
        };
        self.deliver_pending_state_events(extension_id, &mut ops);
        Ok(outcome)
    }

    fn search(
        &mut self,
        extension_id: &ExtensionId,
        contribution_id: &str,
        request: &shilpo_ext_api::bindings::shilpo::extension::types::SearchRequest,
        budget: RuntimeBudget,
    ) -> Result<
        Vec<shilpo_ext_api::bindings::shilpo::extension::types::SearchCandidate>,
        RuntimeError,
    > {
        let span = tracing::info_span!(
            target: "shilpo_profile",
            "extension_wasm_call",
            extension_id = %extension_id,
            operation = "search",
            fuel = budget.fuel,
            memory_limit = budget.max_memory_bytes,
            outcome = "failure",
        );
        let _enter = span.enter();
        let instance = self.instance_mut(extension_id)?;
        Self::prepare_call(instance, budget)?;
        let wit_request = convert_search_request_to_wit(request);
        let result = instance
            .extension
            .call_search(&mut instance.store, contribution_id, &wit_request)
            .map_err(|error| classify_wasmtime_error("search call failed", error))?;
        let mut ops = Vec::new();
        let outcome = match result {
            Ok(candidates) => {
                span.record("outcome", "success");
                candidates
                    .into_iter()
                    .take(64)
                    .map(convert_search_candidate_from_wit)
                    .collect()
            }
            Err(err) => {
                span.record("outcome", "failure");
                return Err(RuntimeError::with_kind(
                    RuntimeFailureKind::InvalidOutput,
                    format!("search failed: {}", err.message),
                ));
            }
        };
        self.deliver_pending_state_events(extension_id, &mut ops);
        Ok(outcome)
    }
}

impl Drop for WasmRuntime {
    fn drop(&mut self) {
        self.ticker_stop.store(true, Ordering::Relaxed);
        if let Some(ticker) = self.ticker.take() {
            let _ = ticker.join();
        }
    }
}

fn configured_engine() -> Result<Engine, RuntimeError> {
    let mut config = Config::new();
    config.wasm_component_model(true);
    config.consume_fuel(true);
    config.epoch_interruption(true);
    config.max_wasm_stack(512 * 1024);
    Engine::new(&config).map_err(|error| {
        RuntimeError::with_kind(
            RuntimeFailureKind::Load,
            format!("failed to configure Wasmtime: {error}"),
        )
    })
}

fn compile_component(engine: &Engine, bytes: &[u8]) -> Result<Component, RuntimeError> {
    Component::new(engine, bytes).map_err(|error| {
        let message = format!("{error:#}");
        let lowercase = message.to_ascii_lowercase();
        if lowercase.contains("component model async")
            || (lowercase.contains("requires") && lowercase.contains("async"))
        {
            return RuntimeError::with_kind(
                RuntimeFailureKind::Load,
                "unsupported async component; rebuild with the synchronous QJS backend".to_string(),
            );
        }
        RuntimeError::with_kind(
            RuntimeFailureKind::Load,
            format!("component compilation failed: {message}"),
        )
    })
}

const ALLOWED_SHILPO_INTERFACES: &[&str] = &[
    "shilpo:extension/actions",
    "shilpo:extension/clipboard",
    "shilpo:extension/filesystem",
    "shilpo:extension/http",
    "shilpo:extension/location",
    "shilpo:extension/notifications",
    "shilpo:extension/state",
    "shilpo:extension/secrets",
    "shilpo:extension/theme",
    "shilpo:extension/wallpaper",
    "shilpo:extension/types",
    "shilpo:extension/events",
    "shilpo:extension/view",
];

const ALLOWED_WASI_INTERFACES: &[&str] = &["wasi:cli/", "wasi:clocks/", "wasi:io/", "wasi:random/"];

fn validate_component_type(engine: &Engine, component: &Component) -> Result<(), RuntimeError> {
    let component_type = component.component_type();
    for (name, _) in component_type.imports(engine) {
        let is_wasi = ALLOWED_WASI_INTERFACES
            .iter()
            .any(|prefix| name.starts_with(prefix));
        let is_valid_shilpo = ALLOWED_SHILPO_INTERFACES.iter().any(|prefix| {
            name == *prefix
                || name.starts_with(&format!("{prefix}@"))
                || name.starts_with(&format!("{prefix}/"))
        });
        if !is_wasi && !is_valid_shilpo {
            return Err(RuntimeError::with_kind(
                RuntimeFailureKind::Load,
                format!("unsupported component import '{name}'"),
            ));
        }
    }
    for export in ["activate", "deactivate", "on-event", "view"] {
        let Some(component_export) = component_type.get_export(engine, export) else {
            return Err(RuntimeError::with_kind(
                RuntimeFailureKind::Load,
                format!("missing required component export '{export}'"),
            ));
        };
        if !matches!(component_export.ty, ComponentItem::ComponentFunc(_)) {
            return Err(RuntimeError::with_kind(
                RuntimeFailureKind::Load,
                format!("component export '{export}' has the wrong WIT type"),
            ));
        }
    }
    // Ask Wasmtime's linker to resolve the component against the generated
    // canonical bindings. Unlike name checks, pre-instantiation compares the
    // complete WIT function/interface types.
    let mut linker = Linker::<WasmState>::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(|error| {
        RuntimeError::with_kind(
            RuntimeFailureKind::Load,
            format!("failed to configure validation WASI linker: {error:#}"),
        )
    })?;
    Extension::add_to_linker::<WasmState, WasmState>(&mut linker, get_wasm_state).map_err(
        |error| {
            RuntimeError::with_kind(
                RuntimeFailureKind::Load,
                format!("failed to configure validation extension linker: {error:#}"),
            )
        },
    )?;
    linker.instantiate_pre(component).map_err(|error| {
        RuntimeError::with_kind(
            RuntimeFailureKind::Load,
            format!("component does not match the canonical WIT contract: {error:#}"),
        )
    })?;
    Ok(())
}

fn classify_wasmtime_error(context: &str, error: wasmtime::Error) -> RuntimeError {
    let message = format!("{error:#}");
    let lowercase = message.to_ascii_lowercase();
    let kind = match error.downcast_ref::<Trap>() {
        Some(Trap::OutOfFuel) => RuntimeFailureKind::FuelExhausted,
        Some(Trap::Interrupt) => RuntimeFailureKind::Timeout,
        Some(Trap::MemoryOutOfBounds | Trap::AllocationTooLarge) => RuntimeFailureKind::MemoryLimit,
        _ if lowercase.contains("fuel") => RuntimeFailureKind::FuelExhausted,
        _ if lowercase.contains("epoch") || lowercase.contains("interrupt") => {
            RuntimeFailureKind::Timeout
        }
        _ if lowercase.contains("memory") || lowercase.contains("resource limit") => {
            RuntimeFailureKind::MemoryLimit
        }
        _ => RuntimeFailureKind::Trap,
    };
    RuntimeError::with_kind(kind, format!("{context}: {message}"))
}

fn data_value_from_json(value: &serde_json::Value) -> self::shilpo::extension::types::DataValue {
    use self::shilpo::extension::types::DataValue;
    match value {
        serde_json::Value::Null => DataValue::None,
        serde_json::Value::Bool(value) => DataValue::BoolValue(*value),
        serde_json::Value::Number(value) => value
            .as_i64()
            .map(DataValue::IntValue)
            .or_else(|| value.as_f64().map(DataValue::FloatValue))
            .unwrap_or_else(|| DataValue::TextValue(value.to_string())),
        serde_json::Value::String(value) => DataValue::TextValue(value.clone()),
        serde_json::Value::Object(object)
            if object.len() == 1
                && object
                    .get("secret_ref")
                    .and_then(|value| value.as_str())
                    .is_some() =>
        {
            DataValue::SecretRef(self::shilpo::extension::types::SecretRef {
                handle: object["secret_ref"].as_str().unwrap().to_owned(),
            })
        }
        value => DataValue::BytesValue(value.to_string().into_bytes()),
    }
}

fn data_value_to_json(value: &self::shilpo::extension::types::DataValue) -> serde_json::Value {
    use self::shilpo::extension::types::DataValue;
    match value {
        DataValue::None => serde_json::Value::Null,
        DataValue::BoolValue(value) => serde_json::Value::Bool(*value),
        DataValue::IntValue(value) => serde_json::json!(*value),
        DataValue::FloatValue(value) => serde_json::json!(*value),
        DataValue::TextValue(value) => serde_json::Value::String(value.clone()),
        DataValue::BytesValue(value) => serde_json::Value::Array(
            value
                .iter()
                .map(|value| serde_json::json!(*value))
                .collect(),
        ),
        DataValue::SecretRef(s) => serde_json::json!({ "secret_ref": s.handle }),
    }
}

/// Maps a store error to the WIT error vocabulary. Messages never include stored
/// values or bytes.
fn map_state_error(error: StateStoreError) -> shilpo::extension::types::Error {
    use self::shilpo::extension::types::ErrorKind;
    let (kind, message) = match error {
        StateStoreError::InvalidKey(message) | StateStoreError::InvalidValue(message) => {
            (ErrorKind::InvalidArgument, message)
        }
        StateStoreError::KeyCountLimit
        | StateStoreError::ValueSizeLimit { .. }
        | StateStoreError::ByteBudgetExceeded { .. } => (ErrorKind::RateLimited, error.to_string()),
        StateStoreError::RevisionOverflow => (ErrorKind::Internal, error.to_string()),
        StateStoreError::Corrupt(message) | StateStoreError::Internal(message) => {
            (ErrorKind::Internal, message)
        }
        StateStoreError::BackendUnavailable(message) => (ErrorKind::BackendUnavailable, message),
    };
    shilpo::extension::types::Error { kind, message }
}

/// Converts a guest data value into a storable value, rejecting non-finite floats
/// (the persisted encoding cannot represent them).
fn stored_value_from_data_value(
    value: &self::shilpo::extension::types::DataValue,
) -> Result<StoredValue, shilpo::extension::types::Error> {
    use self::shilpo::extension::types::DataValue;
    match value {
        DataValue::None => Ok(StoredValue::None),
        DataValue::BoolValue(value) => Ok(StoredValue::Bool(*value)),
        DataValue::IntValue(value) => Ok(StoredValue::Int(*value)),
        DataValue::FloatValue(value) if value.is_finite() => Ok(StoredValue::Float(*value)),
        DataValue::FloatValue(_) => Err(shilpo::extension::types::Error {
            kind: shilpo::extension::types::ErrorKind::InvalidArgument,
            message: "non-finite floats cannot be stored in extension state".into(),
        }),
        DataValue::TextValue(value) => Ok(StoredValue::Text(value.clone())),
        DataValue::BytesValue(value) => Ok(StoredValue::Bytes(value.clone())),
        DataValue::SecretRef(_) => Err(shilpo::extension::types::Error {
            kind: shilpo::extension::types::ErrorKind::InvalidArgument,
            message: "secret references cannot be stored in extension state".into(),
        }),
    }
}

/// Converts a stored value back into a guest data value. Stored values never
/// contain secret references.
/// Serializes a stored value to the JSON representation used for queued state
/// events. Mirrors `data_value_to_json` for stored variants.
fn stored_value_to_json(value: &StoredValue) -> serde_json::Value {
    match value {
        StoredValue::None => serde_json::Value::Null,
        StoredValue::Bool(value) => serde_json::Value::Bool(*value),
        StoredValue::Int(value) => serde_json::json!(*value),
        StoredValue::Float(value) => serde_json::json!(*value),
        StoredValue::Text(value) => serde_json::Value::String(value.clone()),
        StoredValue::Bytes(value) => serde_json::Value::Array(
            value
                .iter()
                .map(|value| serde_json::json!(*value))
                .collect(),
        ),
    }
}

fn data_value_from_stored(value: &StoredValue) -> self::shilpo::extension::types::DataValue {
    use self::shilpo::extension::types::DataValue;
    match value {
        StoredValue::None => DataValue::None,
        StoredValue::Bool(value) => DataValue::BoolValue(*value),
        StoredValue::Int(value) => DataValue::IntValue(*value),
        StoredValue::Float(value) => DataValue::FloatValue(*value),
        StoredValue::Text(value) => DataValue::TextValue(value.clone()),
        StoredValue::Bytes(value) => DataValue::BytesValue(value.clone()),
    }
}

#[cfg(test)]
mod state_seam_tests {
    use std::time::Duration;

    use shilpo_ext_api::ExtensionId;

    use super::*;
    use crate::secrets::FakeSecretBroker;

    fn state() -> WasmState {
        let extension_id = ExtensionId::new("io.github.test.state-seam").unwrap();
        WasmState {
            extension_id,
            declared_capabilities: Vec::new(),
            granted_capabilities: Vec::new(),
            secret_broker: Arc::new(FakeSecretBroker::new()),
            grant_checker: None,
            limits: StoreLimitsBuilder::new().build(),
            table: ResourceTable::new(),
            wasi: WasiCtxBuilder::new().build(),
            operations: Vec::new(),
            state_store: Arc::new(crate::state::FakeStateStore::default()),
            watch_registry: Arc::new(Mutex::new(HashMap::new())),
            pending_state_events: Arc::new(Mutex::new(HashMap::new())),
            next_watch_id: Arc::new(AtomicU64::new(1)),
            state_operation_lock: Arc::new(Mutex::new(())),
            hostcall_bytes: 0,
            max_hostcall_bytes: 1024 * 1024,
            secret_deadline: Instant::now() + Duration::from_secs(5),
        }
    }

    #[test]
    fn current_seam_round_trips_json_values_in_memory() {
        let mut state = state();
        let mutation = <WasmState as shilpo::extension::state::Host>::write(
            &mut state,
            "greeting".into(),
            shilpo::extension::types::DataValue::TextValue("hello".into()),
        )
        .unwrap();
        assert!(mutation.changed);
        assert_eq!(mutation.revision, 1);
        let stored =
            <WasmState as shilpo::extension::state::Host>::read(&mut state, "greeting".into())
                .unwrap();
        assert!(matches!(
            stored.value,
            Some(shilpo::extension::types::DataValue::TextValue(text)) if text == "hello"
        ));
        assert_eq!(stored.revision, 1);
        let missing =
            <WasmState as shilpo::extension::state::Host>::read(&mut state, "absent".into())
                .unwrap();
        assert!(missing.value.is_none());
    }

    #[test]
    fn current_seam_forgets_values_across_instances() {
        let mut first = state();
        <WasmState as shilpo::extension::state::Host>::write(
            &mut first,
            "transient".into(),
            shilpo::extension::types::DataValue::IntValue(7),
        )
        .unwrap();
        let mut second = state();
        let value =
            <WasmState as shilpo::extension::state::Host>::read(&mut second, "transient".into())
                .unwrap();
        assert!(
            value.value.is_none(),
            "characterizes the durability gap: fresh instances lose state"
        );
    }

    fn pending_for(state: &WasmState) -> Vec<PendingStateEvent> {
        state
            .pending_state_events
            .lock()
            .unwrap()
            .get(&state.extension_id)
            .cloned()
            .map(|queue| queue.into_iter().collect())
            .unwrap_or_default()
    }

    fn write_text(state: &mut WasmState, key: &str, text: &str) {
        <WasmState as shilpo::extension::state::Host>::write(
            state,
            key.into(),
            shilpo::extension::types::DataValue::TextValue(text.into()),
        )
        .unwrap();
    }

    fn watch_key(state: &mut WasmState, key: &str) -> u64 {
        <WasmState as shilpo::extension::state::Host>::watch(state, key.into())
            .unwrap()
            .watch_id
    }

    #[test]
    fn watch_registration_snapshots_atomically() {
        let mut state = state();
        write_text(&mut state, "greeting", "hello");
        let registration =
            <WasmState as shilpo::extension::state::Host>::watch(&mut state, "greeting".into())
                .unwrap();
        assert_eq!(registration.watch_id, 1);
        assert_eq!(registration.snapshot.revision, 1);
        assert!(matches!(
            registration.snapshot.value,
            Some(shilpo::extension::types::DataValue::TextValue(text)) if text == "hello"
        ));

        let missing =
            <WasmState as shilpo::extension::state::Host>::watch(&mut state, "absent".into())
                .unwrap();
        assert_eq!(missing.snapshot.revision, 1);
        assert!(missing.snapshot.value.is_none());
    }

    #[test]
    fn changed_write_queues_one_event_per_live_watch() {
        let mut state = state();
        let first = watch_key(&mut state, "greeting");
        let second = watch_key(&mut state, "greeting");
        write_text(&mut state, "greeting", "updated");
        let pending = pending_for(&state);
        assert_eq!(pending.len(), 2);
        assert!(
            pending
                .iter()
                .any(|event| event.watch_id == first && event.revision == 1)
        );
        assert!(
            pending
                .iter()
                .any(|event| event.watch_id == second && event.revision == 1)
        );
        assert!(pending.iter().all(|event| event.key == "greeting"));
        assert!(pending.iter().all(|event| {
            matches!(&event.value, Some(serde_json::Value::String(text)) if text == "updated")
        }));
    }

    #[test]
    fn unwatch_stops_delivery() {
        let mut state = state();
        let watch_id = watch_key(&mut state, "greeting");
        <WasmState as shilpo::extension::state::Host>::unwatch(&mut state, watch_id).unwrap();
        write_text(&mut state, "greeting", "updated");
        assert!(pending_for(&state).is_empty());
    }

    #[test]
    fn delete_queues_none_value_event() {
        let mut state = state();
        write_text(&mut state, "greeting", "hello");
        let watch_id = watch_key(&mut state, "greeting");
        let mutation =
            <WasmState as shilpo::extension::state::Host>::delete(&mut state, "greeting".into())
                .unwrap();
        assert!(mutation.changed);
        let pending = pending_for(&state);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].watch_id, watch_id);
        assert_eq!(pending[0].revision, 2);
        assert!(pending[0].value.is_none());
    }

    #[test]
    fn idempotent_and_unrelated_mutations_do_not_queue() {
        let mut state = state();
        write_text(&mut state, "greeting", "hello");
        let _ = watch_key(&mut state, "greeting");
        // Idempotent rewrite: same bytes, changed = false.
        let mutation = <WasmState as shilpo::extension::state::Host>::write(
            &mut state,
            "greeting".into(),
            shilpo::extension::types::DataValue::TextValue("hello".into()),
        )
        .unwrap();
        assert!(!mutation.changed);
        // Unrelated key.
        write_text(&mut state, "other", "value");
        // Delete of a missing key.
        let deleted =
            <WasmState as shilpo::extension::state::Host>::delete(&mut state, "absent".into())
                .unwrap();
        assert!(!deleted.changed);
        assert!(pending_for(&state).is_empty());
    }

    #[test]
    fn pending_queue_coalesces_to_latest_revision_per_watch() {
        let mut state = state();
        let watch_id = watch_key(&mut state, "greeting");
        let mutations = crate::state::MAX_PENDING_STATE_EVENTS + 20;
        for index in 0..mutations {
            write_text(&mut state, "greeting", &format!("value-{index}"));
        }
        let pending = pending_for(&state);
        assert_eq!(pending.len(), crate::state::MAX_PENDING_STATE_EVENTS);
        let latest = pending.last().expect("watch event survives coalescing");
        assert_eq!(latest.watch_id, watch_id);
        assert_eq!(latest.revision, mutations as u64);
    }

    #[test]
    fn drop_watch_state_clears_registrations_and_pending() {
        let runtime = WasmRuntime::with_broker(Arc::new(FakeSecretBroker::new())).unwrap();
        let extension_id = ExtensionId::new("io.github.test.state-seam").unwrap();
        let mut state = WasmState {
            extension_id: extension_id.clone(),
            declared_capabilities: Vec::new(),
            granted_capabilities: Vec::new(),
            secret_broker: runtime.secret_broker.clone(),
            grant_checker: None,
            limits: StoreLimitsBuilder::new().build(),
            table: ResourceTable::new(),
            wasi: WasiCtxBuilder::new().build(),
            operations: Vec::new(),
            state_store: runtime.state_store.clone(),
            watch_registry: runtime.watch_registry.clone(),
            pending_state_events: runtime.pending_state_events.clone(),
            next_watch_id: runtime.next_watch_id.clone(),
            state_operation_lock: runtime.state_operation_lock.clone(),
            hostcall_bytes: 0,
            max_hostcall_bytes: 1024 * 1024,
            secret_deadline: Instant::now() + Duration::from_secs(5),
        };
        let _ = watch_key(&mut state, "greeting");
        write_text(&mut state, "greeting", "updated");
        assert!(!pending_for(&state).is_empty());
        runtime.drop_watch_state(&extension_id);
        assert!(runtime.watch_registry.lock().unwrap().is_empty());
        assert!(runtime.pending_state_events.lock().unwrap().is_empty());
        assert!(!runtime.instances.contains_key(&extension_id));
    }

    #[test]
    fn secret_refs_are_rejected_by_state_write() {
        let mut state = state();
        let error = <WasmState as shilpo::extension::state::Host>::write(
            &mut state,
            "credential".into(),
            shilpo::extension::types::DataValue::SecretRef(shilpo::extension::types::SecretRef {
                handle: "opaque-handle".into(),
            }),
        )
        .unwrap_err();
        assert_eq!(
            error.kind,
            shilpo::extension::types::ErrorKind::InvalidArgument
        );
        assert!(pending_for(&state).is_empty());
    }

    #[test]
    fn oversized_key_and_value_errors_are_mapped() {
        let mut state = state();
        let long_key = "k".repeat(crate::state::MAX_KEY_BYTES + 1);
        let error =
            <WasmState as shilpo::extension::state::Host>::read(&mut state, long_key.clone())
                .unwrap_err();
        assert_eq!(
            error.kind,
            shilpo::extension::types::ErrorKind::InvalidArgument
        );
        let error = <WasmState as shilpo::extension::state::Host>::write(
            &mut state,
            long_key,
            shilpo::extension::types::DataValue::TextValue("x".into()),
        )
        .unwrap_err();
        assert_eq!(
            error.kind,
            shilpo::extension::types::ErrorKind::InvalidArgument
        );
        let oversized = <WasmState as shilpo::extension::state::Host>::write(
            &mut state,
            "big".into(),
            shilpo::extension::types::DataValue::BytesValue(vec![0; crate::state::MAX_VALUE_BYTES]),
        )
        .unwrap_err();
        assert_eq!(
            oversized.kind,
            shilpo::extension::types::ErrorKind::RateLimited
        );
    }
}

#[cfg(test)]
mod secret_host_tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use shilpo_ext_api::{Capability, ExtensionId, SecretPurpose};

    use super::*;
    use crate::secrets::FakeSecretBroker;

    fn state(granted: bool, checker: Option<GrantChecker>) -> WasmState {
        let extension_id = ExtensionId::new("io.github.test.secret-host").unwrap();
        let purpose = SecretPurpose::parse("api-token").unwrap();
        WasmState {
            extension_id,
            declared_capabilities: vec![Capability::Secrets {
                purposes: vec![purpose.clone()],
            }],
            granted_capabilities: if granted {
                vec![Capability::Secrets {
                    purposes: vec![purpose],
                }]
            } else {
                Vec::new()
            },
            secret_broker: Arc::new(FakeSecretBroker::new()),
            grant_checker: checker,
            limits: StoreLimitsBuilder::new().build(),
            table: ResourceTable::new(),
            wasi: WasiCtxBuilder::new().build(),
            operations: Vec::new(),
            state_store: Arc::new(crate::state::FakeStateStore::default()),
            watch_registry: Arc::new(Mutex::new(HashMap::new())),
            pending_state_events: Arc::new(Mutex::new(HashMap::new())),
            next_watch_id: Arc::new(AtomicU64::new(1)),
            state_operation_lock: Arc::new(Mutex::new(())),
            hostcall_bytes: 0,
            max_hostcall_bytes: 1024 * 1024,
            secret_deadline: Instant::now() + Duration::from_secs(5),
        }
    }

    #[test]
    fn generated_secret_host_set_read_delete_uses_injected_broker() {
        let mut state = state(true, None);
        let reference = <WasmState as shilpo::extension::secrets::Host>::set(
            &mut state,
            "api-token".into(),
            b"sentinel-secret".to_vec(),
        )
        .unwrap();
        let value = <WasmState as shilpo::extension::secrets::Host>::read(
            &mut state,
            "api-token".into(),
            reference.clone(),
        )
        .unwrap();
        assert_eq!(value, Some(b"sentinel-secret".to_vec()));
        <WasmState as shilpo::extension::secrets::Host>::delete(
            &mut state,
            "api-token".into(),
            reference,
        )
        .unwrap();
    }

    #[test]
    fn generated_secret_host_observes_live_revocation() {
        let allowed = Arc::new(AtomicBool::new(true));
        let checker_allowed = allowed.clone();
        let checker: GrantChecker = Arc::new(move |_, scope| {
            scope == "secrets:api-token" && checker_allowed.load(Ordering::Relaxed)
        });
        let mut state = state(true, Some(checker));
        <WasmState as shilpo::extension::secrets::Host>::set(
            &mut state,
            "api-token".into(),
            b"before-revocation".to_vec(),
        )
        .unwrap();
        allowed.store(false, Ordering::Relaxed);
        let error = <WasmState as shilpo::extension::secrets::Host>::set(
            &mut state,
            "api-token".into(),
            b"after-revocation".to_vec(),
        )
        .unwrap_err();
        assert_eq!(
            error.kind,
            shilpo::extension::types::ErrorKind::Unauthorized
        );
    }

    #[test]
    fn secret_refs_cannot_enter_action_effects() {
        let mut state = state(true, None);
        let error = <WasmState as shilpo::extension::actions::Host>::invoke(
            &mut state,
            "test-action".into(),
            Some(shilpo::extension::types::DataValue::SecretRef(
                shilpo::extension::types::SecretRef {
                    handle: "opaque-handle".into(),
                },
            )),
        )
        .unwrap_err();
        assert_eq!(
            error.kind,
            shilpo::extension::types::ErrorKind::InvalidArgument
        );
        assert!(state.operations.is_empty());
    }

    #[test]
    fn secret_refs_cannot_enter_the_state_store() {
        let reference = shilpo::extension::types::SecretRef {
            handle: "opaque-handle".into(),
        };
        let value = shilpo::extension::types::DataValue::SecretRef(reference.clone());
        let json = data_value_to_json(&value);
        assert_eq!(json, serde_json::json!({ "secret_ref": "opaque-handle" }));
        assert!(matches!(
            data_value_from_json(&json),
            shilpo::extension::types::DataValue::SecretRef(value) if value.handle == reference.handle
        ));

        let mut state = state(true, None);
        let error = <WasmState as shilpo::extension::state::Host>::write(
            &mut state,
            "credential".into(),
            value,
        )
        .unwrap_err();
        assert_eq!(
            error.kind,
            shilpo::extension::types::ErrorKind::InvalidArgument
        );
        let stored =
            <WasmState as shilpo::extension::state::Host>::read(&mut state, "credential".into())
                .unwrap();
        assert!(
            stored.value.is_none(),
            "extension state must never become a credential store"
        );
    }

    #[test]
    fn test_bar_menu_event_wit_conversion_all_reasons() {
        use shilpo_ext_api::BarMenuCloseReason;

        let extension_id = ExtensionId::new("org.shilpo.weather").unwrap();
        let opened = shilpo_ext_api::ExtensionEvent::BarMenuOpened {
            contribution_id: "org.shilpo.weather/weather-menu".into(),
            instance_id: "bar:0:center:0".into(),
        };
        let wit_opened = convert_event_to_wit(&extension_id, &opened);
        assert!(matches!(
            wit_opened,
            self::shilpo::extension::events::ExtensionEvent::BarMenuOpened(_)
        ));
        let self::shilpo::extension::events::ExtensionEvent::BarMenuOpened(payload) = &wit_opened
        else {
            unreachable!()
        };
        assert_eq!(payload.contribution_id, "weather-menu");

        let reasons = [
            BarMenuCloseReason::SourceToggle,
            BarMenuCloseReason::Escape,
            BarMenuCloseReason::FocusLost,
            BarMenuCloseReason::OutsideClick,
            BarMenuCloseReason::OverviewOpened,
            BarMenuCloseReason::BarClosed,
            BarMenuCloseReason::DisplayRemoved,
            BarMenuCloseReason::OwnerRemoved,
            BarMenuCloseReason::SourceUnavailable,
        ];
        for reason in reasons {
            let closed = shilpo_ext_api::ExtensionEvent::BarMenuClosed {
                contribution_id: "org.shilpo.weather/weather-menu".into(),
                instance_id: "bar:0:center:0".into(),
                reason,
            };
            let wit_closed = convert_event_to_wit(&extension_id, &closed);
            assert!(matches!(
                wit_closed,
                self::shilpo::extension::events::ExtensionEvent::BarMenuClosed(_)
            ));
            let self::shilpo::extension::events::ExtensionEvent::BarMenuClosed(payload) =
                &wit_closed
            else {
                unreachable!()
            };
            assert_eq!(payload.contribution_id, "weather-menu");
        }
    }
}

/// Strips the `extension_id/` prefix from a canonical contribution id before handing it to
/// the guest. Host-side event routing (`ExtensionSession::dispatch`) needs the full canonical
/// string to determine which extension owns an event, but the guest already knows its own
/// identity and only ever compares against the bare contribution id it declared in its
/// manifest (mirroring `HostContract::render_view`, which passes `canonical.contribution_id`
/// rather than the full canonical string into the guest's `view` export). `extension_id` here
/// is always the target this event was routed to, so the prefix is guaranteed to match.
fn strip_extension_prefix(extension_id: &ExtensionId, contribution_id: &str) -> String {
    contribution_id
        .strip_prefix(extension_id.as_str())
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(contribution_id)
        .to_string()
}

fn convert_event_to_wit(
    extension_id: &ExtensionId,
    event: &ApiEvent,
) -> self::shilpo::extension::events::ExtensionEvent {
    use self::shilpo::extension::events as wit_events;
    match event {
        ApiEvent::ShellStarted => wit_events::ExtensionEvent::ShellStarted,
        ApiEvent::ShellStopping => wit_events::ExtensionEvent::ShellStopping,
        ApiEvent::OutputsChanged => wit_events::ExtensionEvent::OutputsChanged,
        ApiEvent::ThemeChanged { mode } => {
            wit_events::ExtensionEvent::ThemeChanged(wit_events::ThemeEvent { mode: mode.clone() })
        }
        ApiEvent::PaletteGenerated { accent } => {
            wit_events::ExtensionEvent::PaletteGenerated(wit_events::PaletteEvent {
                accent: accent.clone(),
            })
        }
        ApiEvent::WallpaperChanged { path } => {
            wit_events::ExtensionEvent::WallpaperChanged(wit_events::WallpaperEvent {
                path: path.clone(),
            })
        }
        ApiEvent::NetworkChanged { connected } => {
            wit_events::ExtensionEvent::NetworkChanged(wit_events::NetworkEvent {
                connected: *connected,
            })
        }
        ApiEvent::MediaChanged {
            title,
            artist,
            playing,
        } => wit_events::ExtensionEvent::MediaChanged(wit_events::MediaEvent {
            title: title.clone(),
            artist: artist.clone(),
            playing: *playing,
        }),
        ApiEvent::PowerChanged {
            percentage,
            charging,
        } => wit_events::ExtensionEvent::PowerChanged(wit_events::PowerEvent {
            percentage: *percentage,
            charging: *charging,
        }),
        ApiEvent::TimerFired { name } => {
            wit_events::ExtensionEvent::TimerFired(wit_events::TimerEvent { name: name.clone() })
        }
        ApiEvent::ContributionMounted {
            contribution_id,
            instance_id,
            width,
            height,
        } => wit_events::ExtensionEvent::ContributionMounted(wit_events::MountedEvent {
            contribution_id: strip_extension_prefix(extension_id, contribution_id),
            instance_id: instance_id.clone(),
            width: *width,
            height: *height,
        }),
        ApiEvent::ContributionUnmounted {
            contribution_id,
            instance_id,
        } => wit_events::ExtensionEvent::ContributionUnmounted(wit_events::UnmountedEvent {
            contribution_id: strip_extension_prefix(extension_id, contribution_id),
            instance_id: instance_id.clone(),
        }),
        ApiEvent::ContributionResized {
            contribution_id,
            instance_id,
            width,
            height,
        } => wit_events::ExtensionEvent::ContributionResized(wit_events::ResizedEvent {
            contribution_id: strip_extension_prefix(extension_id, contribution_id),
            instance_id: instance_id.clone(),
            width: *width,
            height: *height,
        }),
        ApiEvent::ContributionSettingsChanged {
            contribution_id,
            instance_id,
            settings,
        } => wit_events::ExtensionEvent::ContributionSettingsChanged(wit_events::SettingsEvent {
            contribution_id: strip_extension_prefix(extension_id, contribution_id),
            instance_id: instance_id.clone(),
            settings: data_value_from_json(settings),
        }),
        ApiEvent::BarMenuOpened {
            contribution_id,
            instance_id,
        } => wit_events::ExtensionEvent::BarMenuOpened(wit_events::BarMenuOpenedPayload {
            contribution_id: strip_extension_prefix(extension_id, contribution_id),
            instance_id: instance_id.clone(),
        }),
        ApiEvent::BarMenuClosed {
            contribution_id,
            instance_id,
            reason,
        } => wit_events::ExtensionEvent::BarMenuClosed(wit_events::BarMenuClosedPayload {
            contribution_id: strip_extension_prefix(extension_id, contribution_id),
            instance_id: instance_id.clone(),
            reason: match reason {
                shilpo_ext_api::BarMenuCloseReason::SourceToggle => {
                    wit_events::BarMenuCloseReason::SourceToggle
                }
                shilpo_ext_api::BarMenuCloseReason::Escape => {
                    wit_events::BarMenuCloseReason::Escape
                }
                shilpo_ext_api::BarMenuCloseReason::FocusLost => {
                    wit_events::BarMenuCloseReason::FocusLost
                }
                shilpo_ext_api::BarMenuCloseReason::OutsideClick => {
                    wit_events::BarMenuCloseReason::OutsideClick
                }
                shilpo_ext_api::BarMenuCloseReason::OverviewOpened => {
                    wit_events::BarMenuCloseReason::OverviewOpened
                }
                shilpo_ext_api::BarMenuCloseReason::BarClosed => {
                    wit_events::BarMenuCloseReason::BarClosed
                }
                shilpo_ext_api::BarMenuCloseReason::DisplayRemoved => {
                    wit_events::BarMenuCloseReason::DisplayRemoved
                }
                shilpo_ext_api::BarMenuCloseReason::OwnerRemoved => {
                    wit_events::BarMenuCloseReason::OwnerRemoved
                }
                shilpo_ext_api::BarMenuCloseReason::SourceUnavailable => {
                    wit_events::BarMenuCloseReason::SourceUnavailable
                }
            },
        }),
        ApiEvent::Input {
            contribution_id,
            instance_id,
            event_id,
            value,
        } => wit_events::ExtensionEvent::Input(wit_events::InputEvent {
            contribution_id: strip_extension_prefix(extension_id, contribution_id),
            instance_id: instance_id.clone(),
            event_id: event_id.clone(),
            value: value.as_ref().map(data_value_from_json),
        }),
        ApiEvent::StateValue {
            watch_id,
            key,
            value,
            revision,
        } => wit_events::ExtensionEvent::StateValue(wit_events::StateEvent {
            watch_id: *watch_id,
            key: key.clone(),
            value: value.as_ref().map(data_value_from_json),
            revision: *revision,
        }),
        ApiEvent::HttpResponse {
            request_id,
            status,
            body,
            error,
        } => wit_events::ExtensionEvent::HttpResponse(wit_events::HttpResponseEvent {
            request_id: request_id.clone(),
            status: *status,
            body: body.clone(),
            error: error.clone(),
        }),
        ApiEvent::LocationResponse {
            latitude,
            longitude,
            accuracy_meters,
            error,
        } => wit_events::ExtensionEvent::LocationResponse(wit_events::LocationResponseEvent {
            latitude: *latitude,
            longitude: *longitude,
            accuracy_meters: *accuracy_meters,
            error: error.clone(),
        }),
        ApiEvent::WorkspaceChanged {
            workspace_id,
            workspace_name,
            output_name,
        } => wit_events::ExtensionEvent::WorkspaceChanged(wit_events::WorkspaceEvent {
            workspace_id: workspace_id.clone(),
            workspace_name: workspace_name.clone(),
            output_name: output_name.clone(),
        }),
        ApiEvent::WallpaperRequest(req) => {
            let reason = match req.reason {
                shilpo_ext_api::WallpaperRequestReason::Activate => {
                    self::shilpo::extension::wallpaper::WallpaperRequestReason::Activate
                }
                shilpo_ext_api::WallpaperRequestReason::UserNext => {
                    self::shilpo::extension::wallpaper::WallpaperRequestReason::UserNext
                }
                shilpo_ext_api::WallpaperRequestReason::SlideshowTick => {
                    self::shilpo::extension::wallpaper::WallpaperRequestReason::SlideshowTick
                }
                shilpo_ext_api::WallpaperRequestReason::WorkspaceChanged => {
                    self::shilpo::extension::wallpaper::WallpaperRequestReason::WorkspaceChanged
                }
                shilpo_ext_api::WallpaperRequestReason::SettingsChanged => {
                    self::shilpo::extension::wallpaper::WallpaperRequestReason::SettingsChanged
                }
            };
            let mode = match req.mode {
                shilpo_ext_api::WallpaperMode::Manual => {
                    self::shilpo::extension::wallpaper::WallpaperMode::Manual
                }
                shilpo_ext_api::WallpaperMode::Slideshow => {
                    self::shilpo::extension::wallpaper::WallpaperMode::Slideshow
                }
            };
            let target = match &req.target {
                shilpo_ext_api::WallpaperTarget::Global => {
                    self::shilpo::extension::wallpaper::WallpaperTarget::Global
                }
                shilpo_ext_api::WallpaperTarget::Workspace(w) => {
                    self::shilpo::extension::wallpaper::WallpaperTarget::Workspace(
                        self::shilpo::extension::wallpaper::WorkspaceTarget {
                            workspace_id: w.workspace_id.clone(),
                            output_name: w.output_name.clone(),
                        },
                    )
                }
            };
            wit_events::ExtensionEvent::WallpaperRequest(
                self::shilpo::extension::wallpaper::WallpaperRequest {
                    request_id: req.request_id.clone(),
                    contribution_id: req.contribution_id.clone(),
                    reason,
                    mode,
                    target,
                },
            )
        }
        ApiEvent::WallpaperResult {
            request_id,
            success,
            error,
        } => wit_events::ExtensionEvent::WallpaperResult(wit_events::WallpaperResultEvent {
            request_id: request_id.clone(),
            success: *success,
            error: error.clone(),
        }),
    }
}

#[cfg(test)]
mod bar_menu_component_fixture_tests {
    use shilpo_ext_api::{
        Alignment, BarMenuCloseReason, ContainerDirection, Justification, Overflow,
        SemanticColorToken, ViewNode,
    };

    use super::*;
    use crate::secrets::FakeSecretBroker;

    const FIXTURE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/bar-menu-component/target/wasm32-wasip2/release/",
        "bar_menu_component_fixture.wasm"
    ));

    fn loaded_fixture() -> (WasmRuntime, ExtensionId) {
        let mut runtime =
            WasmRuntime::with_broker(Arc::new(FakeSecretBroker::new())).expect("runtime");
        let extension_id = ExtensionId::new("io.example.fixture").unwrap();
        runtime
            .load(
                &extension_id,
                WasmModule::from_bytes(FIXTURE),
                RuntimeBudget::default(),
            )
            .expect("fixture must instantiate through the canonical component boundary");
        (runtime, extension_id)
    }

    #[test]
    fn guest_component_round_trips_bar_menu_tree_and_lifecycle_matrix() {
        let (mut runtime, extension_id) = loaded_fixture();
        let tree = runtime
            .view(&extension_id, "menu", RuntimeBudget::default())
            .expect("view call")
            .expect("menu fixture view");

        let ViewNode::Container(container) = tree.root else {
            panic!("fixture root must remain a container");
        };
        assert_eq!(container.direction, ContainerDirection::Grid { columns: 2 });
        assert_eq!(container.children.len(), 2);
        assert_eq!(container.gap, Some(8.0));
        assert_eq!(container.align_items, Some(Alignment::Center));
        assert_eq!(container.justify_content, Some(Justification::SpaceBetween));
        assert_eq!(container.event_id.as_deref(), Some("background"));
        let style = container.style.expect("fixture style");
        assert_eq!(style.padding, Some(12.0));
        assert_eq!(style.margin, Some(2.0));
        assert_eq!(style.corner_radius, Some(16.0));
        assert_eq!(style.opacity, Some(0.95));
        assert_eq!(style.color, Some(SemanticColorToken::OnSurface));
        assert_eq!(style.background, Some(SemanticColorToken::SurfaceContainer));
        assert_eq!(style.border_width, Some(1.0));
        assert_eq!(style.border_color, Some(SemanticColorToken::Outline));
        assert_eq!(style.min_width, None);
        assert_eq!(style.max_width, None);
        assert_eq!(style.min_height, None);
        assert_eq!(style.max_height, None);
        assert_eq!(style.overflow, Some(Overflow::Scroll));

        let contribution_id = "io.example.fixture/menu".to_owned();
        let instance_id = "bar:display-1:fixture".to_owned();
        runtime
            .dispatch(
                &extension_id,
                &ApiEvent::BarMenuOpened {
                    contribution_id: contribution_id.clone(),
                    instance_id: instance_id.clone(),
                },
                RuntimeBudget::default(),
            )
            .expect("opened event must cross WIT");

        for reason in [
            BarMenuCloseReason::SourceToggle,
            BarMenuCloseReason::Escape,
            BarMenuCloseReason::FocusLost,
            BarMenuCloseReason::OutsideClick,
            BarMenuCloseReason::OverviewOpened,
            BarMenuCloseReason::BarClosed,
            BarMenuCloseReason::DisplayRemoved,
            BarMenuCloseReason::OwnerRemoved,
            BarMenuCloseReason::SourceUnavailable,
        ] {
            runtime
                .dispatch(
                    &extension_id,
                    &ApiEvent::BarMenuClosed {
                        contribution_id: contribution_id.clone(),
                        instance_id: instance_id.clone(),
                        reason,
                    },
                    RuntimeBudget::default(),
                )
                .expect("every close reason must cross WIT");
        }
    }

    const SDK_FIXTURE: &[u8] = include_bytes!(env!("SHILPO_SDK_FIXTURE_WASM"));

    #[test]
    fn sdk_authored_guest_component_instantiates_and_renders_view_tree() {
        let mut runtime =
            WasmRuntime::with_broker(Arc::new(FakeSecretBroker::new())).expect("runtime");
        let extension_id = ExtensionId::new("io.example.sdk-fixture").unwrap();
        runtime
            .load(
                &extension_id,
                WasmModule::from_bytes(SDK_FIXTURE),
                RuntimeBudget::default(),
            )
            .expect(
                "SDK-authored fixture must instantiate through the canonical component boundary",
            );

        let tree = runtime
            .view(&extension_id, "widget", RuntimeBudget::default())
            .expect("view call")
            .expect("sdk fixture view");

        let ViewNode::Container(container) = tree.root else {
            panic!("fixture root must remain a container");
        };
        assert_eq!(container.direction, ContainerDirection::Grid { columns: 2 });
        assert_eq!(container.children.len(), 2);

        let ViewNode::Container(row) = &container.children[0] else {
            panic!("first child must be row container");
        };
        assert_eq!(row.direction, ContainerDirection::Row);
        assert_eq!(row.children.len(), 2);

        let ViewNode::Icon(icon) = &row.children[0] else {
            panic!("first row child must be icon");
        };
        assert_eq!(icon.name, "star");
        assert_eq!(icon.size, Some(16.0));

        let ViewNode::Text(text) = &row.children[1] else {
            panic!("second row child must be text");
        };
        assert_eq!(text.content, "Count: 42");
        assert_eq!(text.bold, Some(true));

        let ViewNode::Button(button) = &container.children[1] else {
            panic!("second child must be button");
        };
        assert_eq!(button.label, "+1");
        assert_eq!(button.event_id, "increment");

        // Dispatch input event to increment count
        runtime
            .dispatch(
                &extension_id,
                &ApiEvent::Input {
                    contribution_id: "widget".into(),
                    instance_id: None,
                    event_id: "increment".into(),
                    value: None,
                },
                RuntimeBudget::default(),
            )
            .expect("input event must succeed");

        let updated_tree = runtime
            .view(&extension_id, "widget", RuntimeBudget::default())
            .expect("view call")
            .expect("sdk fixture view");

        let ViewNode::Container(updated_container) = updated_tree.root else {
            panic!("updated root must remain a container");
        };
        let ViewNode::Container(updated_row) = &updated_container.children[0] else {
            panic!("first child must be row container");
        };
        let ViewNode::Text(updated_text) = &updated_row.children[1] else {
            panic!("second row child must be text");
        };
        assert_eq!(updated_text.content, "Count: 43");
    }
}

fn convert_search_request_to_wit(
    req: &shilpo_ext_api::bindings::shilpo::extension::types::SearchRequest,
) -> self::shilpo::extension::types::SearchRequest {
    use self::shilpo::extension::types as wit_types;
    use shilpo_ext_api::bindings::shilpo::extension::types as api_types;

    let mode = match req.mode {
        api_types::SearchMode::Default => wit_types::SearchMode::Default,
        api_types::SearchMode::Apps => wit_types::SearchMode::Apps,
        api_types::SearchMode::Actions => wit_types::SearchMode::Actions,
        api_types::SearchMode::Clipboard => wit_types::SearchMode::Clipboard,
        api_types::SearchMode::Calculator => wit_types::SearchMode::Calculator,
        api_types::SearchMode::Command => wit_types::SearchMode::Command,
        api_types::SearchMode::WebSearch => wit_types::SearchMode::WebSearch,
        api_types::SearchMode::Keybindings => wit_types::SearchMode::Keybindings,
    };
    wit_types::SearchRequest {
        raw_query: req.raw_query.clone(),
        query: req.query.clone(),
        mode,
        generation: req.generation,
    }
}

fn convert_search_candidate_from_wit(
    cand: self::shilpo::extension::types::SearchCandidate,
) -> shilpo_ext_api::bindings::shilpo::extension::types::SearchCandidate {
    use self::shilpo::extension::types as wit_types;
    use shilpo_ext_api::bindings::shilpo::extension::types as api_types;

    let category = match cand.category {
        wit_types::SearchResultCategory::Window => api_types::SearchResultCategory::Window,
        wit_types::SearchResultCategory::Application => {
            api_types::SearchResultCategory::Application
        }
        wit_types::SearchResultCategory::Action => api_types::SearchResultCategory::Action,
        wit_types::SearchResultCategory::Clipboard => api_types::SearchResultCategory::Clipboard,
        wit_types::SearchResultCategory::Calculator => api_types::SearchResultCategory::Calculator,
        wit_types::SearchResultCategory::Command => api_types::SearchResultCategory::Command,
        wit_types::SearchResultCategory::WebSearch => api_types::SearchResultCategory::WebSearch,
        wit_types::SearchResultCategory::FilePath => api_types::SearchResultCategory::FilePath,
        wit_types::SearchResultCategory::Uri => api_types::SearchResultCategory::Uri,
        wit_types::SearchResultCategory::Keybinding => api_types::SearchResultCategory::Keybinding,
        wit_types::SearchResultCategory::Custom => api_types::SearchResultCategory::Custom,
    };
    let icon = cand.icon.map(|i| match i {
        wit_types::SearchIcon::None => api_types::SearchIcon::None,
        wit_types::SearchIcon::Named(s) => api_types::SearchIcon::Named(s),
        wit_types::SearchIcon::Asset(s) => api_types::SearchIcon::Asset(s),
    });
    api_types::SearchCandidate {
        id: cand.id,
        title: cand.title,
        subtitle: cand.subtitle,
        aliases: cand.aliases,
        keywords: cand.keywords,
        category,
        icon,
        activation_verb: cand.activation_verb,
        activation_payload: cand.activation_payload,
    }
}

fn convert_view_tree_from_wit(
    tree: self::shilpo::extension::view::ViewTree,
) -> Result<ApiViewTree, RuntimeError> {
    fn decode_node(
        idx: usize,
        nodes: &[self::shilpo::extension::view::ViewNode],
    ) -> Result<shilpo_ext_api::ViewNode, RuntimeError> {
        use shilpo_ext_api as api;

        use self::shilpo::extension::view as wit_view;
        if idx >= nodes.len() {
            return Err(RuntimeError::with_kind(
                RuntimeFailureKind::InvalidOutput,
                format!("invalid node index {idx} in view tree"),
            ));
        }
        let node = match &nodes[idx] {
            wit_view::ViewNode::Container(c) => {
                let mut children = Vec::with_capacity(c.children.len());
                for &child_idx in &c.children {
                    children.push(decode_node(child_idx as usize, nodes)?);
                }
                api::ViewNode::Container(api::ContainerNode {
                    direction: match &c.direction {
                        wit_view::ContainerDirection::Row => api::ContainerDirection::Row,
                        wit_view::ContainerDirection::Column => api::ContainerDirection::Column,
                        wit_view::ContainerDirection::Stack => api::ContainerDirection::Stack,
                        wit_view::ContainerDirection::Grid(cols) => {
                            api::ContainerDirection::Grid { columns: *cols }
                        }
                    },
                    children,
                    style: c.style.as_ref().map(decode_style),
                    gap: c.gap,
                    align_items: c.align_items.map(|a| match a {
                        wit_view::Alignment::Start => api::Alignment::Start,
                        wit_view::Alignment::Center => api::Alignment::Center,
                        wit_view::Alignment::End => api::Alignment::End,
                        wit_view::Alignment::Stretch => api::Alignment::Stretch,
                    }),
                    justify_content: c.justify_content.map(|j| match j {
                        wit_view::Justification::Start => api::Justification::Start,
                        wit_view::Justification::Center => api::Justification::Center,
                        wit_view::Justification::End => api::Justification::End,
                        wit_view::Justification::SpaceBetween => api::Justification::SpaceBetween,
                        wit_view::Justification::SpaceAround => api::Justification::SpaceAround,
                    }),
                    wrap: c.wrap,
                    event_id: c.event_id.clone(),
                })
            }
            wit_view::ViewNode::Text(t) => api::ViewNode::Text(api::TextNode {
                content: t.content.clone(),
                font_size: t.font_size,
                bold: t.bold,
                style: t.style.as_ref().map(decode_style),
            }),
            wit_view::ViewNode::Icon(i) => api::ViewNode::Icon(api::IconNode {
                name: i.name.clone(),
                size: i.size,
                style: i.style.as_ref().map(decode_style),
            }),
            wit_view::ViewNode::Image(img) => api::ViewNode::Image(api::ImageNode {
                asset_path: img.asset_path.clone(),
                width: img.width,
                height: img.height,
                style: img.style.as_ref().map(decode_style),
            }),
            wit_view::ViewNode::Button(b) => api::ViewNode::Button(api::ButtonNode {
                label: b.label.clone(),
                event_id: b.event_id.clone(),
                style: b.style.as_ref().map(decode_style),
            }),
            wit_view::ViewNode::IconButton(ib) => api::ViewNode::IconButton(api::IconButtonNode {
                icon_name: ib.icon_name.clone(),
                event_id: ib.event_id.clone(),
                style: ib.style.as_ref().map(decode_style),
            }),
            wit_view::ViewNode::Toggle(t) => api::ViewNode::Toggle(api::ToggleNode {
                value: t.value,
                event_id: t.event_id.clone(),
                style: t.style.as_ref().map(decode_style),
            }),
            wit_view::ViewNode::Slider(s) => api::ViewNode::Slider(api::SliderNode {
                value: s.value,
                min: s.min,
                max: s.max,
                event_id: s.event_id.clone(),
                style: s.style.as_ref().map(decode_style),
            }),
            wit_view::ViewNode::TextInput(ti) => api::ViewNode::TextInput(api::TextInputNode {
                placeholder: ti.placeholder.clone(),
                value: ti.value.clone(),
                event_id: ti.event_id.clone(),
                style: ti.style.as_ref().map(decode_style),
            }),
            wit_view::ViewNode::List(l) => {
                let mut items = Vec::with_capacity(l.items.len());
                for &item_idx in &l.items {
                    items.push(decode_node(item_idx as usize, nodes)?);
                }
                api::ViewNode::List(api::ListNode {
                    items,
                    style: l.style.as_ref().map(decode_style),
                })
            }
            wit_view::ViewNode::Spacer(sp) => {
                api::ViewNode::Spacer(api::SpacerNode { size: sp.size })
            }
            wit_view::ViewNode::Divider => api::ViewNode::Divider,
            wit_view::ViewNode::Badge(bd) => api::ViewNode::Badge(api::BadgeNode {
                label: bd.label.clone(),
                style: bd.style.as_ref().map(decode_style),
            }),
            wit_view::ViewNode::Progress(pr) => api::ViewNode::Progress(api::ProgressNode {
                value: pr.value,
                style: pr.style.as_ref().map(decode_style),
            }),
            wit_view::ViewNode::LoadingIndicator(li) => {
                api::ViewNode::LoadingIndicator(api::LoadingIndicatorNode {
                    size: li.size,
                    color: li.color.map(decode_color),
                    style: li.style.as_ref().map(decode_style),
                })
            }
        };
        Ok(node)
    }

    fn decode_style(s: &self::shilpo::extension::view::ViewStyle) -> shilpo_ext_api::ViewStyle {
        use shilpo_ext_api as api;

        use self::shilpo::extension::view as wit_view;
        shilpo_ext_api::ViewStyle {
            padding: s.padding,
            margin: s.margin,
            width: s.width,
            height: s.height,
            corner_radius: s.corner_radius,
            opacity: s.opacity,
            color: s.color.map(decode_color),
            background: s.background.map(decode_color),
            flex_grow: s.flex_grow,
            border_width: s.border_width,
            border_color: s.border_color.map(decode_color),
            min_width: s.min_width,
            max_width: s.max_width,
            min_height: s.min_height,
            max_height: s.max_height,
            overflow: s.overflow.map(|o| match o {
                wit_view::Overflow::Visible => api::Overflow::Visible,
                wit_view::Overflow::Hidden => api::Overflow::Hidden,
                wit_view::Overflow::Scroll => api::Overflow::Scroll,
            }),
        }
    }

    fn decode_color(
        token: self::shilpo::extension::view::SemanticColorToken,
    ) -> shilpo_ext_api::SemanticColorToken {
        use shilpo_ext_api as api;

        use self::shilpo::extension::view as wit_view;
        match token {
            wit_view::SemanticColorToken::Surface => api::SemanticColorToken::Surface,
            wit_view::SemanticColorToken::OnSurface => api::SemanticColorToken::OnSurface,
            wit_view::SemanticColorToken::SurfaceDim => api::SemanticColorToken::SurfaceDim,
            wit_view::SemanticColorToken::SurfaceBright => api::SemanticColorToken::SurfaceBright,
            wit_view::SemanticColorToken::SurfaceContainerLowest => {
                api::SemanticColorToken::SurfaceContainerLowest
            }
            wit_view::SemanticColorToken::SurfaceContainerLow => {
                api::SemanticColorToken::SurfaceContainerLow
            }
            wit_view::SemanticColorToken::SurfaceContainer => {
                api::SemanticColorToken::SurfaceContainer
            }
            wit_view::SemanticColorToken::SurfaceContainerHigh => {
                api::SemanticColorToken::SurfaceContainerHigh
            }
            wit_view::SemanticColorToken::SurfaceContainerHighest => {
                api::SemanticColorToken::SurfaceContainerHighest
            }
            wit_view::SemanticColorToken::SurfaceVariant => {
                api::SemanticColorToken::SurfaceVariant
            }
            wit_view::SemanticColorToken::OnSurfaceVariant => {
                api::SemanticColorToken::OnSurfaceVariant
            }
            wit_view::SemanticColorToken::InverseSurface => {
                api::SemanticColorToken::InverseSurface
            }
            wit_view::SemanticColorToken::InverseOnSurface => {
                api::SemanticColorToken::InverseOnSurface
            }
            wit_view::SemanticColorToken::Outline => api::SemanticColorToken::Outline,
            wit_view::SemanticColorToken::OutlineVariant => {
                api::SemanticColorToken::OutlineVariant
            }
            wit_view::SemanticColorToken::Shadow => api::SemanticColorToken::Shadow,
            wit_view::SemanticColorToken::Scrim => api::SemanticColorToken::Scrim,
            wit_view::SemanticColorToken::SurfaceTint => api::SemanticColorToken::SurfaceTint,
            wit_view::SemanticColorToken::Primary => api::SemanticColorToken::Primary,
            wit_view::SemanticColorToken::OnPrimary => api::SemanticColorToken::OnPrimary,
            wit_view::SemanticColorToken::PrimaryContainer => {
                api::SemanticColorToken::PrimaryContainer
            }
            wit_view::SemanticColorToken::OnPrimaryContainer => {
                api::SemanticColorToken::OnPrimaryContainer
            }
            wit_view::SemanticColorToken::InversePrimary => {
                api::SemanticColorToken::InversePrimary
            }
            wit_view::SemanticColorToken::PrimaryFixed => api::SemanticColorToken::PrimaryFixed,
            wit_view::SemanticColorToken::PrimaryFixedDim => {
                api::SemanticColorToken::PrimaryFixedDim
            }
            wit_view::SemanticColorToken::OnPrimaryFixed => {
                api::SemanticColorToken::OnPrimaryFixed
            }
            wit_view::SemanticColorToken::OnPrimaryFixedVariant => {
                api::SemanticColorToken::OnPrimaryFixedVariant
            }
            wit_view::SemanticColorToken::Secondary => api::SemanticColorToken::Secondary,
            wit_view::SemanticColorToken::OnSecondary => api::SemanticColorToken::OnSecondary,
            wit_view::SemanticColorToken::SecondaryContainer => {
                api::SemanticColorToken::SecondaryContainer
            }
            wit_view::SemanticColorToken::OnSecondaryContainer => {
                api::SemanticColorToken::OnSecondaryContainer
            }
            wit_view::SemanticColorToken::SecondaryFixed => {
                api::SemanticColorToken::SecondaryFixed
            }
            wit_view::SemanticColorToken::SecondaryFixedDim => {
                api::SemanticColorToken::SecondaryFixedDim
            }
            wit_view::SemanticColorToken::OnSecondaryFixed => {
                api::SemanticColorToken::OnSecondaryFixed
            }
            wit_view::SemanticColorToken::OnSecondaryFixedVariant => {
                api::SemanticColorToken::OnSecondaryFixedVariant
            }
            wit_view::SemanticColorToken::Tertiary => api::SemanticColorToken::Tertiary,
            wit_view::SemanticColorToken::OnTertiary => api::SemanticColorToken::OnTertiary,
            wit_view::SemanticColorToken::TertiaryContainer => {
                api::SemanticColorToken::TertiaryContainer
            }
            wit_view::SemanticColorToken::OnTertiaryContainer => {
                api::SemanticColorToken::OnTertiaryContainer
            }
            wit_view::SemanticColorToken::TertiaryFixed => {
                api::SemanticColorToken::TertiaryFixed
            }
            wit_view::SemanticColorToken::TertiaryFixedDim => {
                api::SemanticColorToken::TertiaryFixedDim
            }
            wit_view::SemanticColorToken::OnTertiaryFixed => {
                api::SemanticColorToken::OnTertiaryFixed
            }
            wit_view::SemanticColorToken::OnTertiaryFixedVariant => {
                api::SemanticColorToken::OnTertiaryFixedVariant
            }
            wit_view::SemanticColorToken::Error => api::SemanticColorToken::Error,
            wit_view::SemanticColorToken::OnError => api::SemanticColorToken::OnError,
            wit_view::SemanticColorToken::ErrorContainer => {
                api::SemanticColorToken::ErrorContainer
            }
            wit_view::SemanticColorToken::OnErrorContainer => {
                api::SemanticColorToken::OnErrorContainer
            }
        }
    }

    let root = decode_node(tree.root as usize, &tree.nodes)?;
    Ok(ApiViewTree::new(root))
}

#[cfg(test)]
mod wit_conversion_tests {
    use self::shilpo::extension::view as wit_view;
    use super::*;

    #[test]
    fn convert_view_tree_preserves_grid_alignment_style_and_container_event_id() {
        let wit_tree = wit_view::ViewTree {
            nodes: vec![wit_view::ViewNode::Container(wit_view::ContainerNode {
                direction: wit_view::ContainerDirection::Grid(4),
                children: vec![],
                style: Some(wit_view::ViewStyle {
                    padding: Some(10.0),
                    margin: None,
                    width: None,
                    height: None,
                    corner_radius: None,
                    opacity: Some(0.8),
                    color: Some(wit_view::SemanticColorToken::Primary),
                    background: Some(wit_view::SemanticColorToken::SurfaceContainer),
                    flex_grow: None,
                    border_width: Some(2.0),
                    border_color: Some(wit_view::SemanticColorToken::Outline),
                    min_width: Some(100.0),
                    max_width: Some(500.0),
                    min_height: Some(50.0),
                    max_height: Some(200.0),
                    overflow: Some(wit_view::Overflow::Scroll),
                }),
                gap: Some(8.0),
                align_items: Some(wit_view::Alignment::Center),
                justify_content: Some(wit_view::Justification::SpaceBetween),
                wrap: false,
                event_id: Some("grid_card".into()),
            })],
            root: 0,
        };

        let converted = convert_view_tree_from_wit(wit_tree).expect("conversion should succeed");
        let shilpo_ext_api::ViewNode::Container(container) = converted.root else {
            panic!("root must be container");
        };

        assert_eq!(
            container.direction,
            shilpo_ext_api::ContainerDirection::Grid { columns: 4 }
        );
        assert_eq!(container.gap, Some(8.0));
        assert_eq!(
            container.align_items,
            Some(shilpo_ext_api::Alignment::Center)
        );
        assert_eq!(
            container.justify_content,
            Some(shilpo_ext_api::Justification::SpaceBetween)
        );
        assert_eq!(container.event_id, Some("grid_card".into()));

        let style = container.style.expect("style present");
        assert_eq!(style.border_width, Some(2.0));
        assert_eq!(
            style.border_color,
            Some(shilpo_ext_api::SemanticColorToken::Outline)
        );
        assert_eq!(style.min_width, Some(100.0));
        assert_eq!(style.max_width, Some(500.0));
        assert_eq!(style.min_height, Some(50.0));
        assert_eq!(style.max_height, Some(200.0));
        assert_eq!(style.overflow, Some(shilpo_ext_api::Overflow::Scroll));
    }

    #[test]
    fn convert_view_tree_preserves_all_directions_and_grid_boundaries() {
        let directions = [
            (
                wit_view::ContainerDirection::Row,
                shilpo_ext_api::ContainerDirection::Row,
            ),
            (
                wit_view::ContainerDirection::Column,
                shilpo_ext_api::ContainerDirection::Column,
            ),
            (
                wit_view::ContainerDirection::Stack,
                shilpo_ext_api::ContainerDirection::Stack,
            ),
        ];
        for (wit_direction, api_direction) in directions {
            let tree = wit_view::ViewTree {
                nodes: vec![wit_view::ViewNode::Container(wit_view::ContainerNode {
                    direction: wit_direction,
                    children: vec![],
                    style: None,
                    gap: None,
                    align_items: None,
                    justify_content: None,
                    wrap: false,
                    event_id: None,
                })],
                root: 0,
            };
            let converted = convert_view_tree_from_wit(tree).unwrap();
            let shilpo_ext_api::ViewNode::Container(container) = converted.root else {
                panic!("root must be container");
            };
            assert_eq!(container.direction, api_direction);
        }

        for columns in [1, 64] {
            let tree = wit_view::ViewTree {
                nodes: vec![wit_view::ViewNode::Container(wit_view::ContainerNode {
                    direction: wit_view::ContainerDirection::Grid(columns),
                    children: vec![],
                    style: None,
                    gap: None,
                    align_items: None,
                    justify_content: None,
                    wrap: false,
                    event_id: None,
                })],
                root: 0,
            };
            let converted = convert_view_tree_from_wit(tree).unwrap();
            let shilpo_ext_api::ViewNode::Container(container) = converted.root else {
                panic!("root must be container");
            };
            assert_eq!(
                container.direction,
                shilpo_ext_api::ContainerDirection::Grid { columns }
            );
        }
    }

    #[test]
    fn invalid_root_index_fails_closed() {
        let wit_tree = wit_view::ViewTree {
            nodes: vec![],
            root: 0,
        };
        let res = convert_view_tree_from_wit(wit_tree);
        assert!(res.is_err());
    }

    #[test]
    fn invalid_child_index_fails_closed() {
        let wit_tree = wit_view::ViewTree {
            nodes: vec![wit_view::ViewNode::Container(wit_view::ContainerNode {
                direction: wit_view::ContainerDirection::Row,
                children: vec![99],
                style: None,
                gap: None,
                align_items: None,
                justify_content: None,
                wrap: false,
                event_id: None,
            })],
            root: 0,
        };
        let res = convert_view_tree_from_wit(wit_tree);
        assert!(res.is_err());
    }
}
