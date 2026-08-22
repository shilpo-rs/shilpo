use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use shilpo_ext_api::{
    CanonicalId, Capability, ContributionId, ExtensionEvent, ExtensionId, ExtensionManifest,
    HostOperation, IdError, ManifestError, TextNode, ViewLimits, ViewNode, ViewTree,
    ViewValidationError,
};

use crate::effects::{AuthorizedHostOperation, capability_allows_operation};
use crate::{CircuitBreaker, DiagnosticCode, ExtensionDiagnostic};

pub type GrantChecker = Arc<dyn Fn(&ExtensionId, &str) -> bool + Send + Sync>;
use std::time::Duration;

pub trait GuestExtension: Send + Sync {
    fn on_event(&mut self, event: &ExtensionEvent) -> Vec<HostOperation>;
    fn view(&self, contribution_id: &str) -> Option<ViewTree>;
    fn search(
        &self,
        contribution_id: &str,
        request: &shilpo_ext_api::bindings::shilpo::extension::types::SearchRequest,
    ) -> Vec<shilpo_ext_api::bindings::shilpo::extension::types::SearchCandidate> {
        let _ = (contribution_id, request);
        Vec::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeFailureKind {
    Load,
    Trap,
    Timeout,
    FuelExhausted,
    MemoryLimit,
    InvalidOutput,
    Unavailable,
}

#[derive(Clone, Debug)]
pub struct RuntimeError {
    kind: RuntimeFailureKind,
    message: String,
}

impl RuntimeError {
    pub fn new(message: impl Into<String>) -> Self {
        Self::with_kind(RuntimeFailureKind::Unavailable, message)
    }

    pub fn with_kind(kind: RuntimeFailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> RuntimeFailureKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}

impl std::error::Error for RuntimeError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeBudget {
    pub max_memory_bytes: usize,
    pub fuel: u64,
    pub deadline: Duration,
    pub max_hostcall_bytes: usize,
    pub max_output_bytes: usize,
}

impl Default for RuntimeBudget {
    fn default() -> Self {
        Self {
            max_memory_bytes: 32 * 1024 * 1024,
            fuel: 10_000_000,
            deadline: Duration::from_millis(100),
            max_hostcall_bytes: 1024 * 1024,
            max_output_bytes: 1024 * 1024,
        }
    }
}

/// Execution boundary used by the policy-owning extension host.
///
/// An in-process adapter is supplied for tests and development. A WASM adapter can
/// implement the same contract without moving policy into the guest runtime.
pub trait ExtensionRuntime {
    type Module;

    /// Install a live grant lookup used by host imports. Adapters without
    /// host-import capability enforcement may ignore this hook.
    fn set_grant_checker(&mut self, _checker: GrantChecker) {}

    /// Load a module with the manifest's declared capabilities and the user's
    /// grants available to host-import authorization. Runtimes that do not
    /// expose host imports may use the default implementation.
    fn load_with_capabilities(
        &mut self,
        extension_id: &ExtensionId,
        module: Self::Module,
        budget: RuntimeBudget,
        _declared_capabilities: Vec<Capability>,
        _granted_capabilities: Vec<Capability>,
    ) -> Result<(), RuntimeError> {
        self.load(extension_id, module, budget)
    }

    /// Replace a module while preserving the manifest/grant context used by
    /// host-import authorization.
    fn replace_with_capabilities(
        &mut self,
        extension_id: &ExtensionId,
        module: Self::Module,
        budget: RuntimeBudget,
        _declared_capabilities: Vec<Capability>,
        _granted_capabilities: Vec<Capability>,
    ) -> Result<(), RuntimeError> {
        self.replace(extension_id, module, budget)
    }

    /// Validates that a compiled module can be instantiated with its grants without
    /// changing the active runtime instance. This is the reload transaction's
    /// preflight seam; adapters that cannot stage an instance may keep the default.
    fn validate_module_with_capabilities(
        &mut self,
        _extension_id: &ExtensionId,
        _module: &Self::Module,
        _budget: RuntimeBudget,
        _declared_capabilities: Vec<Capability>,
        _granted_capabilities: Vec<Capability>,
    ) -> Result<(), RuntimeError> {
        Ok(())
    }

    fn load(
        &mut self,
        extension_id: &ExtensionId,
        module: Self::Module,
        budget: RuntimeBudget,
    ) -> Result<(), RuntimeError>;
    /// Validate and stage a replacement before making it active. Implementations
    /// must leave the existing instance untouched when staging fails.
    fn replace(
        &mut self,
        extension_id: &ExtensionId,
        module: Self::Module,
        budget: RuntimeBudget,
    ) -> Result<(), RuntimeError>;
    fn unload(&mut self, extension_id: &ExtensionId) -> Result<(), RuntimeError>;
    fn dispatch(
        &mut self,
        extension_id: &ExtensionId,
        event: &ExtensionEvent,
        budget: RuntimeBudget,
    ) -> Result<Vec<HostOperation>, RuntimeError>;
    fn view(
        &mut self,
        extension_id: &ExtensionId,
        contribution_id: &str,
        budget: RuntimeBudget,
    ) -> Result<Option<ViewTree>, RuntimeError>;
    fn search(
        &mut self,
        extension_id: &ExtensionId,
        contribution_id: &str,
        request: &shilpo_ext_api::bindings::shilpo::extension::types::SearchRequest,
        budget: RuntimeBudget,
    ) -> Result<
        Vec<shilpo_ext_api::bindings::shilpo::extension::types::SearchCandidate>,
        RuntimeError,
    >;
    fn compile_module(&self, bytes: &[u8]) -> Result<Self::Module, String>;
}

#[derive(Default)]
pub struct InMemoryRuntime {
    guests: HashMap<ExtensionId, Box<dyn GuestExtension>>,
}

impl InMemoryRuntime {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ExtensionRuntime for InMemoryRuntime {
    type Module = Box<dyn GuestExtension>;

    fn compile_module(&self, bytes: &[u8]) -> Result<Self::Module, String> {
        if bytes.is_empty() {
            return Err("empty WASM bytes".to_owned());
        }
        #[derive(Default)]
        struct DummyInMemoryGuest;
        impl GuestExtension for DummyInMemoryGuest {
            fn on_event(&mut self, _event: &ExtensionEvent) -> Vec<HostOperation> {
                Vec::new()
            }
            fn view(&self, _contribution_id: &str) -> Option<ViewTree> {
                Some(ViewTree::new(ViewNode::Text(TextNode {
                    content: "dummy".into(),
                    ..Default::default()
                })))
            }
        }
        Ok(Box::new(DummyInMemoryGuest))
    }

    fn load(
        &mut self,
        extension_id: &ExtensionId,
        module: Self::Module,
        _budget: RuntimeBudget,
    ) -> Result<(), RuntimeError> {
        if self.guests.insert(extension_id.clone(), module).is_some() {
            return Err(RuntimeError::new(format!(
                "extension '{extension_id}' is already loaded"
            )));
        }
        Ok(())
    }

    fn replace(
        &mut self,
        extension_id: &ExtensionId,
        module: Self::Module,
        _budget: RuntimeBudget,
    ) -> Result<(), RuntimeError> {
        if !self.guests.contains_key(extension_id) {
            return Err(RuntimeError::new(format!(
                "extension '{extension_id}' is not loaded"
            )));
        }
        self.guests.insert(extension_id.clone(), module);
        Ok(())
    }

    fn unload(&mut self, extension_id: &ExtensionId) -> Result<(), RuntimeError> {
        self.guests
            .remove(extension_id)
            .map(|_| ())
            .ok_or_else(|| RuntimeError::new(format!("extension '{extension_id}' is not loaded")))
    }

    fn dispatch(
        &mut self,
        extension_id: &ExtensionId,
        event: &ExtensionEvent,
        _budget: RuntimeBudget,
    ) -> Result<Vec<HostOperation>, RuntimeError> {
        self.guests
            .get_mut(extension_id)
            .map(|guest| guest.on_event(event))
            .ok_or_else(|| RuntimeError::new(format!("extension '{extension_id}' is not loaded")))
    }

    fn view(
        &mut self,
        extension_id: &ExtensionId,
        contribution_id: &str,
        _budget: RuntimeBudget,
    ) -> Result<Option<ViewTree>, RuntimeError> {
        self.guests
            .get(extension_id)
            .map(|guest| guest.view(contribution_id))
            .ok_or_else(|| RuntimeError::new(format!("extension '{extension_id}' is not loaded")))
    }

    fn search(
        &mut self,
        extension_id: &ExtensionId,
        contribution_id: &str,
        request: &shilpo_ext_api::bindings::shilpo::extension::types::SearchRequest,
        _budget: RuntimeBudget,
    ) -> Result<
        Vec<shilpo_ext_api::bindings::shilpo::extension::types::SearchCandidate>,
        RuntimeError,
    > {
        self.guests
            .get(extension_id)
            .map(|guest| guest.search(contribution_id, request))
            .ok_or_else(|| RuntimeError::new(format!("extension '{extension_id}' is not loaded")))
    }
}

#[derive(Debug)]
pub enum HostError {
    Manifest(ManifestError),
    Runtime(RuntimeError),
    View(ViewValidationError),
    AlreadyRegistered(ExtensionId),
    NotRegistered(ExtensionId),
    UnknownContribution(CanonicalId),
    UndeclaredGrant(String),
    Disabled(ExtensionId),
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(error) => error.fmt(f),
            Self::Runtime(error) => error.fmt(f),
            Self::View(error) => write!(f, "invalid extension view: {error}"),
            Self::AlreadyRegistered(id) => write!(f, "extension '{id}' is already registered"),
            Self::NotRegistered(id) => write!(f, "extension '{id}' is not registered"),
            Self::UnknownContribution(id) => write!(f, "unknown extension contribution '{id}'"),
            Self::UndeclaredGrant(kind) => {
                write!(f, "cannot grant undeclared capability '{kind}'")
            }
            Self::Disabled(id) => {
                write!(f, "extension '{id}' is disabled for this session")
            }
        }
    }
}

impl std::error::Error for HostError {}

impl From<ManifestError> for HostError {
    fn from(value: ManifestError) -> Self {
        Self::Manifest(value)
    }
}

impl From<IdError> for HostError {
    fn from(value: IdError) -> Self {
        Self::Manifest(ManifestError::Id(value))
    }
}

impl From<RuntimeError> for HostError {
    fn from(value: RuntimeError) -> Self {
        Self::Runtime(value)
    }
}

impl From<ViewValidationError> for HostError {
    fn from(value: ViewValidationError) -> Self {
        Self::View(value)
    }
}

struct Registration {
    manifest: ExtensionManifest,
    grants: Vec<Capability>,
}

#[derive(Debug, Default, PartialEq)]
pub struct DispatchResult {
    pub accepted: Vec<AuthorizedHostOperation>,
    pub rejected: Vec<HostOperation>,
}

use crate::circuit_breaker::{CircuitBreakerPolicy, MonotonicClock};

pub struct ExtensionHost<R> {
    runtime: R,
    registrations: HashMap<ExtensionId, Registration>,
    view_limits: ViewLimits,
    runtime_budget: RuntimeBudget,
    circuit_breaker: CircuitBreaker,
}

impl<R: ExtensionRuntime> ExtensionHost<R> {
    pub fn new(runtime: R) -> Self {
        Self {
            runtime,
            registrations: HashMap::new(),
            view_limits: ViewLimits::default(),
            runtime_budget: RuntimeBudget::default(),
            circuit_breaker: CircuitBreaker::default(),
        }
    }

    pub fn runtime(&self) -> &R {
        &self.runtime
    }

    pub fn runtime_mut(&mut self) -> &mut R {
        &mut self.runtime
    }

    pub fn clear(&mut self) {
        let ids: Vec<_> = self.registrations.keys().cloned().collect();
        for id in ids {
            let _ = self.runtime.unload(&id);
        }
        self.registrations.clear();
        self.circuit_breaker.clear();
    }

    pub fn with_view_limits(mut self, limits: ViewLimits) -> Self {
        self.view_limits = limits;
        self
    }

    pub fn with_runtime_budget(mut self, budget: RuntimeBudget) -> Self {
        self.runtime_budget = budget;
        self
    }

    pub fn with_failure_threshold(mut self, max_consecutive_failures: u32) -> Self {
        self.circuit_breaker = CircuitBreaker::new_with_threshold(max_consecutive_failures);
        self
    }

    pub fn with_circuit_breaker_policy(mut self, policy: CircuitBreakerPolicy) -> Self {
        self.circuit_breaker = self.circuit_breaker.with_policy(policy);
        self
    }

    pub fn with_clock(mut self, clock: Arc<dyn MonotonicClock>) -> Self {
        self.circuit_breaker = self.circuit_breaker.with_clock(clock);
        self
    }

    pub fn circuit_breaker(&self) -> &CircuitBreaker {
        &self.circuit_breaker
    }

    pub fn circuit_breaker_mut(&mut self) -> &mut CircuitBreaker {
        &mut self.circuit_breaker
    }

    pub fn runtime_budget(&self) -> RuntimeBudget {
        self.runtime_budget
    }

    pub fn register(
        &mut self,
        manifest: ExtensionManifest,
        module: R::Module,
        grants: Vec<Capability>,
    ) -> Result<(), HostError> {
        manifest.validate()?;
        if self.registrations.contains_key(&manifest.id) {
            return Err(HostError::AlreadyRegistered(manifest.id));
        }
        for grant in &grants {
            if !manifest.capabilities.iter().any(|requested| {
                requested.kind() == grant.kind()
                    && match (requested, grant) {
                        (
                            Capability::Secrets { purposes: declared },
                            Capability::Secrets { purposes: granted },
                        ) => granted.iter().all(|purpose| declared.contains(purpose)),
                        _ => true,
                    }
            }) {
                return Err(HostError::UndeclaredGrant(format!("{:?}", grant.kind())));
            }
        }

        let id = manifest.id.clone();
        if let Err(error) = self.runtime.load_with_capabilities(
            &id,
            module,
            self.runtime_budget,
            manifest.capabilities.clone(),
            grants.clone(),
        ) {
            self.record_runtime_failure(&id, &error);
            return Err(error.into());
        }
        self.registrations
            .insert(id.clone(), Registration { manifest, grants });
        self.circuit_breaker.reset(&id);
        Ok(())
    }

    pub fn unregister(&mut self, id: &ExtensionId) -> Result<(), HostError> {
        if !self.registrations.contains_key(id) {
            return Err(HostError::NotRegistered(id.clone()));
        }
        if let Err(error) = self.runtime.unload(id) {
            self.record_runtime_failure(id, &error);
            return Err(error.into());
        }
        self.registrations.remove(id);
        self.circuit_breaker.remove(id);
        Ok(())
    }

    pub fn replace(
        &mut self,
        manifest: ExtensionManifest,
        module: R::Module,
        grants: Vec<Capability>,
    ) -> Result<(), HostError> {
        manifest.validate()?;
        let id = manifest.id.clone();
        if !self.registrations.contains_key(&id) {
            return Err(HostError::NotRegistered(id));
        }
        for grant in &grants {
            if !manifest.capabilities.iter().any(|requested| {
                requested.kind() == grant.kind()
                    && match (requested, grant) {
                        (
                            Capability::Secrets { purposes: declared },
                            Capability::Secrets { purposes: granted },
                        ) => granted.iter().all(|purpose| declared.contains(purpose)),
                        _ => true,
                    }
            }) {
                return Err(HostError::UndeclaredGrant(format!("{:?}", grant.kind())));
            }
        }

        if let Err(error) = self.runtime.replace_with_capabilities(
            &id,
            module,
            self.runtime_budget,
            manifest.capabilities.clone(),
            grants.clone(),
        ) {
            self.record_runtime_failure(&id, &error);
            return Err(HostError::Runtime(error));
        }
        self.registrations
            .insert(id.clone(), Registration { manifest, grants });
        self.circuit_breaker.reset(&id);
        Ok(())
    }

    pub fn dispatch_event(
        &mut self,
        extension_id: &ExtensionId,
        event: &ExtensionEvent,
    ) -> Result<DispatchResult, HostError> {
        let _permit = self.circuit_breaker.acquire_permit(extension_id)?;
        let registration = match self.registrations.get(extension_id) {
            Some(r) => r,
            None => {
                self.circuit_breaker.release_probe(extension_id);
                return Err(HostError::NotRegistered(extension_id.clone()));
            }
        };

        if let Some(event_kind) = event.subscription_kind() {
            let subscribed = registration
                .manifest
                .subscriptions
                .iter()
                .any(|subscription| subscription.event == event_kind);
            let declared = registration
                .manifest
                .capabilities
                .iter()
                .any(|capability| capability.allows_event(event_kind));
            let granted = registration
                .grants
                .iter()
                .any(|capability| capability.allows_event(event_kind));
            if !(subscribed && declared && granted) {
                self.circuit_breaker.release_probe(extension_id);
                return Ok(DispatchResult::default());
            }
        }

        if let ExtensionEvent::Input {
            contribution_id, ..
        } = event
        {
            let contribution_id = match ContributionId::new(contribution_id.clone()) {
                Ok(id) => id,
                Err(err) => {
                    self.circuit_breaker.release_probe(extension_id);
                    return Err(err.into());
                }
            };
            if !registration
                .manifest
                .contributions
                .contains(&contribution_id)
            {
                self.circuit_breaker.release_probe(extension_id);
                return Err(HostError::UnknownContribution(CanonicalId::new(
                    extension_id.clone(),
                    contribution_id,
                )));
            }
        }

        let operations = match self
            .runtime
            .dispatch(extension_id, event, self.runtime_budget)
        {
            Ok(ops) => ops,
            Err(error) => {
                self.record_runtime_failure(extension_id, &error);
                return Err(error.into());
            }
        };
        let mut result = DispatchResult::default();
        for op in operations {
            match authorize_operation(op, registration) {
                Ok(authorized) => result.accepted.push(authorized),
                Err(rejected) => result.rejected.push(rejected),
            }
        }
        if result.rejected.is_empty() {
            self.circuit_breaker.record_success(extension_id);
        } else {
            self.circuit_breaker.record_failure(
                extension_id,
                DiagnosticCode::CapabilityDenied,
                format!(
                    "rejected {} operation(s) outside the extension's declared and granted capabilities",
                    result.rejected.len()
                ),
            );
        }
        Ok(result)
    }

    pub fn render_view(&mut self, canonical: &CanonicalId) -> Result<Option<ViewTree>, HostError> {
        let _permit = self
            .circuit_breaker
            .acquire_permit(&canonical.extension_id)?;
        let registration = match self.registrations.get(&canonical.extension_id) {
            Some(r) => r,
            None => {
                self.circuit_breaker.release_probe(&canonical.extension_id);
                return Err(HostError::NotRegistered(canonical.extension_id.clone()));
            }
        };
        if !registration
            .manifest
            .contributions
            .contains(&canonical.contribution_id)
        {
            self.circuit_breaker.release_probe(&canonical.extension_id);
            return Err(HostError::UnknownContribution(canonical.clone()));
        }
        let view = match self.runtime.view(
            &canonical.extension_id,
            canonical.contribution_id.as_str(),
            self.runtime_budget,
        ) {
            Ok(view) => view,
            Err(error) => {
                self.record_runtime_failure(&canonical.extension_id, &error);
                return Err(error.into());
            }
        };
        if let Some(view) = &view
            && let Err(error) = view.validate(self.view_limits)
        {
            self.circuit_breaker.record_failure(
                &canonical.extension_id,
                DiagnosticCode::InvalidView,
                error.to_string(),
            );
            return Err(error.into());
        }
        self.circuit_breaker.record_success(&canonical.extension_id);
        Ok(view)
    }

    pub fn search(
        &mut self,
        canonical: &CanonicalId,
        request: &shilpo_ext_api::bindings::shilpo::extension::types::SearchRequest,
        budget: RuntimeBudget,
    ) -> Result<Vec<shilpo_ext_api::bindings::shilpo::extension::types::SearchCandidate>, HostError>
    {
        let _permit = self
            .circuit_breaker
            .acquire_permit(&canonical.extension_id)?;
        let registration = match self.registrations.get(&canonical.extension_id) {
            Some(r) => r,
            None => {
                self.circuit_breaker.release_probe(&canonical.extension_id);
                return Err(HostError::NotRegistered(canonical.extension_id.clone()));
            }
        };
        if !registration
            .manifest
            .contributions
            .search_providers
            .iter()
            .any(|sp| sp.id == canonical.contribution_id)
        {
            self.circuit_breaker.release_probe(&canonical.extension_id);
            return Err(HostError::UnknownContribution(canonical.clone()));
        }
        let candidates = match self.runtime.search(
            &canonical.extension_id,
            canonical.contribution_id.as_str(),
            request,
            budget,
        ) {
            Ok(candidates) => candidates,
            Err(error) => {
                self.record_runtime_failure(&canonical.extension_id, &error);
                return Err(error.into());
            }
        };
        self.circuit_breaker.record_success(&canonical.extension_id);
        Ok(candidates)
    }

    /// Records a failure against this extension's circuit breaker for a timeout the
    /// worker never itself observed — the caller (the shell's coordinator) gave up
    /// waiting for a reply before this process could produce one. This keeps a single
    /// authoritative circuit breaker per extension: the shell has no safe way to mutate
    /// breaker state directly since it lives in this out-of-process worker, so it must
    /// route "I gave up waiting" back through a command instead of maintaining its own
    /// parallel copy.
    pub fn record_coordinator_timeout(&mut self, id: &ExtensionId) {
        self.circuit_breaker.record_failure(
            id,
            DiagnosticCode::RuntimeTimeout,
            "coordinator timed out waiting for a reply before this worker responded",
        );
    }

    pub fn manifest(&self, id: &ExtensionId) -> Option<&ExtensionManifest> {
        self.registrations
            .get(id)
            .map(|registration| &registration.manifest)
    }

    pub fn diagnostics(&self) -> &[ExtensionDiagnostic] {
        self.circuit_breaker.diagnostics()
    }

    pub fn reset_extension(&mut self, id: &ExtensionId) {
        self.circuit_breaker.reset(id);
    }

    pub fn is_disabled(&self, id: &ExtensionId) -> bool {
        self.circuit_breaker.is_disabled(id)
    }

    fn record_runtime_failure(&mut self, id: &ExtensionId, error: &RuntimeError) {
        self.circuit_breaker
            .record_failure(id, diagnostic_code(error.kind()), error.to_string());
    }
}

impl Default for ExtensionHost<InMemoryRuntime> {
    fn default() -> Self {
        Self::new(InMemoryRuntime::default())
    }
}

fn diagnostic_code(kind: RuntimeFailureKind) -> DiagnosticCode {
    match kind {
        RuntimeFailureKind::Load | RuntimeFailureKind::Unavailable => DiagnosticCode::RuntimeLoad,
        RuntimeFailureKind::Trap => DiagnosticCode::RuntimeTrap,
        RuntimeFailureKind::Timeout => DiagnosticCode::RuntimeTimeout,
        RuntimeFailureKind::FuelExhausted => DiagnosticCode::FuelExhausted,
        RuntimeFailureKind::MemoryLimit => DiagnosticCode::MemoryLimit,
        RuntimeFailureKind::InvalidOutput => DiagnosticCode::InvalidOutput,
    }
}

fn authorize_operation(
    operation: HostOperation,
    registration: &Registration,
) -> Result<AuthorizedHostOperation, HostOperation> {
    match operation {
        HostOperation::HttpRequest {
            request_id,
            url,
            method,
        } => {
            let target = match crate::effects::CanonicalHttpTarget::parse(&url, &method) {
                Some(target) => target,
                None => {
                    return Err(HostOperation::HttpRequest {
                        request_id,
                        url,
                        method,
                    });
                }
            };
            let declared = registration
                .manifest
                .capabilities
                .iter()
                .any(|capability| crate::capability_allows_http_target(capability, &target));
            let granted = registration
                .grants
                .iter()
                .any(|capability| crate::capability_allows_http_target(capability, &target));
            if declared && granted {
                Ok(AuthorizedHostOperation::http_request(request_id, target))
            } else {
                Err(HostOperation::HttpRequest {
                    request_id,
                    url,
                    method,
                })
            }
        }
        operation => {
            let allowed = registration
                .manifest
                .capabilities
                .iter()
                .any(|capability| capability_allows_operation(capability, &operation))
                && registration
                    .grants
                    .iter()
                    .any(|capability| capability_allows_operation(capability, &operation));
            if allowed {
                AuthorizedHostOperation::non_http(operation)
            } else {
                Err(operation)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::circuit_breaker::{CircuitStateKind, FakeMonotonicClock};

    struct ConfigurableFailureRuntime {
        failure_kind: Option<RuntimeFailureKind>,
    }

    impl ExtensionRuntime for ConfigurableFailureRuntime {
        type Module = ();

        fn compile_module(&self, _bytes: &[u8]) -> Result<Self::Module, String> {
            Ok(())
        }

        fn load(
            &mut self,
            _id: &ExtensionId,
            _m: Self::Module,
            _b: RuntimeBudget,
        ) -> Result<(), RuntimeError> {
            if let Some(RuntimeFailureKind::Load) = self.failure_kind {
                return Err(RuntimeError::with_kind(
                    RuntimeFailureKind::Load,
                    "simulated load failure",
                ));
            }
            Ok(())
        }

        fn replace(
            &mut self,
            _id: &ExtensionId,
            _m: Self::Module,
            _b: RuntimeBudget,
        ) -> Result<(), RuntimeError> {
            if let Some(RuntimeFailureKind::Load) = self.failure_kind {
                return Err(RuntimeError::with_kind(
                    RuntimeFailureKind::Load,
                    "simulated replace failure",
                ));
            }
            Ok(())
        }

        fn unload(&mut self, _id: &ExtensionId) -> Result<(), RuntimeError> {
            Ok(())
        }

        fn dispatch(
            &mut self,
            _id: &ExtensionId,
            _event: &ExtensionEvent,
            _b: RuntimeBudget,
        ) -> Result<Vec<HostOperation>, RuntimeError> {
            if let Some(kind) = self.failure_kind {
                return Err(RuntimeError::with_kind(kind, "simulated dispatch failure"));
            }
            Ok(Vec::new())
        }

        fn view(
            &mut self,
            _id: &ExtensionId,
            _cid: &str,
            _b: RuntimeBudget,
        ) -> Result<Option<ViewTree>, RuntimeError> {
            if let Some(kind) = self.failure_kind {
                return Err(RuntimeError::with_kind(kind, "simulated view failure"));
            }
            Ok(None)
        }

        fn search(
            &mut self,
            _id: &ExtensionId,
            _cid: &str,
            _req: &shilpo_ext_api::bindings::shilpo::extension::types::SearchRequest,
            _b: RuntimeBudget,
        ) -> Result<
            Vec<shilpo_ext_api::bindings::shilpo::extension::types::SearchCandidate>,
            RuntimeError,
        > {
            if let Some(kind) = self.failure_kind {
                return Err(RuntimeError::with_kind(kind, "simulated search failure"));
            }
            Ok(Vec::new())
        }
    }

    #[test]
    fn test_failure_classification_counting_and_rejection() {
        let kinds = [
            (RuntimeFailureKind::Trap, DiagnosticCode::RuntimeTrap),
            (RuntimeFailureKind::Timeout, DiagnosticCode::RuntimeTimeout),
            (
                RuntimeFailureKind::FuelExhausted,
                DiagnosticCode::FuelExhausted,
            ),
            (RuntimeFailureKind::MemoryLimit, DiagnosticCode::MemoryLimit),
            (
                RuntimeFailureKind::InvalidOutput,
                DiagnosticCode::InvalidOutput,
            ),
        ];

        for (kind, expected_code) in kinds {
            let clock = Arc::new(FakeMonotonicClock::new(Instant::now()));
            let mut host = ExtensionHost::new(ConfigurableFailureRuntime {
                failure_kind: Some(kind),
            })
            .with_failure_threshold(3)
            .with_clock(clock);

            let manifest = ExtensionManifest::from_toml(
                r#"
                id = "io.github.test.fail"
                name = "Fail"
                version = "1.0.0"
                "#,
            )
            .unwrap();
            let id = manifest.id.clone();
            host.register(manifest, (), vec![]).unwrap();

            // 1st failure
            assert!(
                host.dispatch_event(&id, &ExtensionEvent::ShellStarted)
                    .is_err()
            );
            assert_eq!(
                host.circuit_breaker().status(&id).consecutive_failures,
                Some(1)
            );

            // 2nd failure
            assert!(
                host.dispatch_event(&id, &ExtensionEvent::ShellStarted)
                    .is_err()
            );
            assert_eq!(
                host.circuit_breaker().status(&id).consecutive_failures,
                Some(2)
            );

            // 3rd failure -> Opens circuit
            assert!(
                host.dispatch_event(&id, &ExtensionEvent::ShellStarted)
                    .is_err()
            );
            assert_eq!(
                host.circuit_breaker().status(&id).state,
                CircuitStateKind::Open
            );

            // Blocked calls do NOT increment failures
            assert!(matches!(
                host.dispatch_event(&id, &ExtensionEvent::ShellStarted),
                Err(HostError::Disabled(_))
            ));
            assert_eq!(host.circuit_breaker().status(&id).trip_count, 1);
            assert_eq!(
                host.diagnostics().last().unwrap().code,
                DiagnosticCode::CircuitOpen
            );
            assert!(host.diagnostics().iter().any(|d| d.code == expected_code));
        }
    }

    #[test]
    fn test_successful_replacement_resets_circuit_while_failed_replacement_retains_state() {
        let clock = Arc::new(FakeMonotonicClock::new(Instant::now()));
        let mut host = ExtensionHost::new(ConfigurableFailureRuntime {
            failure_kind: Some(RuntimeFailureKind::Trap),
        })
        .with_failure_threshold(1)
        .with_clock(clock);

        let manifest = ExtensionManifest::from_toml(
            r#"
            id = "io.github.test.replace"
            name = "Replace"
            version = "1.0.0"
            "#,
        )
        .unwrap();
        let id = manifest.id.clone();
        host.register(manifest.clone(), (), vec![]).unwrap();

        // Dispatch fails -> Tripped to Open
        assert!(
            host.dispatch_event(&id, &ExtensionEvent::ShellStarted)
                .is_err()
        );
        assert_eq!(
            host.circuit_breaker().status(&id).state,
            CircuitStateKind::Open
        );

        // Failed replacement retains Open circuit state
        host.runtime_mut().failure_kind = Some(RuntimeFailureKind::Load);
        let replace_err = host.replace(manifest.clone(), (), vec![]);
        assert!(replace_err.is_err());
        assert_eq!(
            host.circuit_breaker().status(&id).state,
            CircuitStateKind::Open
        );

        // Successful replacement resets to clean closed state
        host.runtime_mut().failure_kind = None;
        let replace_ok = host.replace(manifest, (), vec![]);
        assert!(replace_ok.is_ok());
        assert_eq!(
            host.circuit_breaker().status(&id).state,
            CircuitStateKind::Closed
        );
        assert_eq!(
            host.circuit_breaker().status(&id).consecutive_failures,
            Some(0)
        );
    }

    #[test]
    fn test_record_coordinator_timeout_trips_circuit_breaker() {
        // #256: the shell's coordinator has no safe way to mutate this breaker directly
        // (it lives in the out-of-process worker), so a coordinator-side timeout is
        // routed back through `ExtensionCommand::RecordSearchTimeout`, which the worker
        // handles by calling this method. Proven here at the layer that actually owns
        // the breaker; the supervisor-level test only proves the command gets dispatched.
        let clock = Arc::new(FakeMonotonicClock::new(Instant::now()));
        let mut host = ExtensionHost::new(ConfigurableFailureRuntime { failure_kind: None })
            .with_failure_threshold(3)
            .with_clock(clock);

        let manifest = ExtensionManifest::from_toml(
            r#"
            id = "io.github.test.search-timeout"
            name = "Search Timeout"
            version = "1.0.0"
            "#,
        )
        .unwrap();
        let id = manifest.id.clone();
        host.register(manifest, (), vec![]).unwrap();

        host.record_coordinator_timeout(&id);
        assert_eq!(
            host.circuit_breaker().status(&id).consecutive_failures,
            Some(1)
        );

        host.record_coordinator_timeout(&id);
        assert_eq!(
            host.circuit_breaker().status(&id).consecutive_failures,
            Some(2)
        );

        host.record_coordinator_timeout(&id);
        assert_eq!(
            host.circuit_breaker().status(&id).state,
            CircuitStateKind::Open
        );
        assert_eq!(
            host.diagnostics().last().unwrap().code,
            DiagnosticCode::CircuitOpen
        );
        assert!(
            host.diagnostics()
                .iter()
                .any(|d| d.code == DiagnosticCode::RuntimeTimeout)
        );
    }
}
