use std::{
    collections::{BTreeMap, HashMap},
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, UNIX_EPOCH},
};

use semver::Version;
use shilpo_ext_api::{
    CanonicalId, Capability, ExtensionEvent, ExtensionId, ExtensionManifest, HostOperation,
    ViewTree,
};

use super::process::HostGeneration;
use super::protocol::{
    ContributionDescriptor, ContributionInstance, ContributionSurface, DevReloadOutcome,
    ExtensionChanges, ExtensionCommand, ExtensionGeneration, ExtensionRuntimeKind,
    ExtensionSnapshot, ExtensionUpdate, ReplaceableEvent,
};
use crate::{
    CURRENT_SHILPO_VERSION, CatalogPaths, ExtensionCatalog, ExtensionHost, ExtensionRuntime,
    MonotonicClock, SystemMonotonicClock,
};

#[derive(Clone, Debug, PartialEq)]
pub struct ActiveSource {
    pub manifest: ExtensionManifest,
    pub root: PathBuf,
    pub grants: Vec<Capability>,
    pub fingerprint: u64,
}

pub struct ExtensionSession<R> {
    pub host: ExtensionHost<R>,
    pub manifests: BTreeMap<ExtensionId, ExtensionManifest>,
    pub runtime_ids: Vec<ExtensionId>,
    pub views: HashMap<CanonicalId, ViewTree>,
    pub instances: HashMap<String, ContributionInstance>,
    pub state: HashMap<(ExtensionId, String), serde_json::Value>,
}

impl<R: ExtensionRuntime> ExtensionSession<R> {
    pub fn new(runtime: R) -> Self {
        Self {
            host: ExtensionHost::new(runtime),
            manifests: BTreeMap::new(),
            runtime_ids: Vec::new(),
            views: HashMap::new(),
            instances: HashMap::new(),
            state: HashMap::new(),
        }
    }

    pub fn clear(&mut self) {
        self.host.clear();
        self.manifests.clear();
        self.runtime_ids.clear();
        self.views.clear();
        self.instances.clear();
        self.state.clear();
    }

    pub fn register(
        &mut self,
        manifest: ExtensionManifest,
        module: R::Module,
        grants: Vec<Capability>,
    ) -> Result<(), String> {
        let id = manifest.id.clone();
        self.host
            .register(manifest.clone(), module, grants)
            .map_err(|error| error.to_string())?;

        for contribution in &manifest.contributions.bar_widgets {
            let canonical = CanonicalId::new(id.clone(), contribution.id.clone());
            if let Ok(Some(view)) = self.host.render_view(&canonical) {
                self.views.insert(canonical, view);
            }
        }
        for contribution in &manifest.contributions.bar_menus {
            let canonical = CanonicalId::new(id.clone(), contribution.id.clone());
            if let Ok(Some(view)) = self.host.render_view(&canonical) {
                self.views.insert(canonical, view);
            }
        }
        for contribution in &manifest.contributions.desktop_widgets {
            let canonical = CanonicalId::new(id.clone(), contribution.id.clone());
            if let Ok(Some(view)) = self.host.render_view(&canonical) {
                self.views.insert(canonical, view);
            }
        }

        self.manifests.insert(id.clone(), manifest);
        self.runtime_ids.push(id);
        Ok(())
    }

    pub fn dispatch(&mut self, event: &ExtensionEvent) -> ExtensionChanges {
        let mut changes = ExtensionChanges::default();
        let target_ids = match event {
            ExtensionEvent::ContributionMounted {
                contribution_id,
                instance_id,
                width,
                height,
            } => {
                if let Ok(canonical) = contribution_id.parse::<CanonicalId>() {
                    let instance_key = instance_id.clone().unwrap_or_else(|| canonical.to_string());
                    self.instances.insert(
                        instance_key,
                        ContributionInstance {
                            id: instance_id.clone().unwrap_or_else(|| canonical.to_string()),
                            contribution: canonical.clone(),
                            output: None,
                            width: *width,
                            height: *height,
                            settings: serde_json::Value::Object(serde_json::Map::new()),
                        },
                    );
                    vec![canonical.extension_id]
                } else {
                    Vec::new()
                }
            }
            ExtensionEvent::ContributionUnmounted {
                contribution_id,
                instance_id,
            } => {
                if let Ok(canonical) = contribution_id.parse::<CanonicalId>() {
                    let instance_key = instance_id.clone().unwrap_or_else(|| canonical.to_string());
                    self.instances.remove(&instance_key);
                    vec![canonical.extension_id]
                } else {
                    Vec::new()
                }
            }
            ExtensionEvent::ContributionResized {
                contribution_id,
                instance_id,
                width,
                height,
            } => {
                if let Ok(canonical) = contribution_id.parse::<CanonicalId>() {
                    let instance_key = instance_id.clone().unwrap_or_else(|| canonical.to_string());
                    if let Some(instance) = self.instances.get_mut(&instance_key) {
                        instance.width = *width;
                        instance.height = *height;
                    }
                    vec![canonical.extension_id]
                } else {
                    Vec::new()
                }
            }
            ExtensionEvent::ContributionSettingsChanged {
                contribution_id,
                instance_id,
                settings,
            } => {
                if let Ok(canonical) = contribution_id.parse::<CanonicalId>() {
                    let instance_key = instance_id.clone().unwrap_or_else(|| canonical.to_string());
                    if let Some(instance) = self.instances.get_mut(&instance_key) {
                        instance.settings = settings.clone();
                    }
                    vec![canonical.extension_id]
                } else {
                    Vec::new()
                }
            }
            ExtensionEvent::Input {
                contribution_id, ..
            } => {
                if let Ok(canonical) = contribution_id.parse::<CanonicalId>() {
                    vec![canonical.extension_id]
                } else {
                    Vec::new()
                }
            }
            ExtensionEvent::BarMenuOpened {
                contribution_id, ..
            }
            | ExtensionEvent::BarMenuClosed {
                contribution_id, ..
            } => {
                if let Ok(canonical) = contribution_id.parse::<CanonicalId>() {
                    vec![canonical.extension_id]
                } else {
                    Vec::new()
                }
            }
            _ => self.runtime_ids.clone(),
        };

        for id in target_ids {
            if let Ok(result) = self.host.dispatch_event(&id, event) {
                for authorized in result.accepted {
                    self.apply_effect(&id, authorized.kind(), &mut changes);
                }
                self.refresh_event_views(event, &id, &mut changes);
            }
        }
        changes
    }

    /// Re-renders whatever contribution(s) an event may have changed. Events scoped to a single
    /// contribution (`Input`, `BarMenuOpened`, `BarMenuClosed`) only refresh that one. Every other
    /// event -- including `HttpResponse` and `LocationResponse`, which carry no `contribution_id`
    /// at all because they answer an extension-wide async request rather than target a specific
    /// widget -- refreshes every contribution the extension owns. Without this, an extension whose
    /// view depends on the outcome of an async host operation (a location lookup, an HTTP fetch, a
    /// periodic `TimerFired` refresh) updates its internal state correctly but the bar never learns
    /// to re-render it, leaving the widget stuck on its initial view forever.
    fn refresh_event_views(
        &mut self,
        event: &ExtensionEvent,
        extension_id: &ExtensionId,
        changes: &mut ExtensionChanges,
    ) {
        match event {
            ExtensionEvent::Input {
                contribution_id, ..
            } => {
                let Ok(canonical) = contribution_id.parse::<CanonicalId>() else {
                    return;
                };
                if &canonical.extension_id != extension_id {
                    return;
                }
                self.refresh_view(&canonical, changes);
            }
            // Unlike `Input`, whose effect is normally localized to the specific
            // contribution the interactive element belongs to, a menu opening or closing
            // can plausibly change how a *different* contribution renders -- e.g. a bar
            // widget that wants to show an "active" appearance while its own menu is open.
            // There's no way to know from the event alone which other views (if any) care,
            // so -- same reasoning as the catch-all branch below -- refresh everything this
            // extension owns rather than assume only the menu's own view is affected.
            ExtensionEvent::BarMenuOpened { .. } | ExtensionEvent::BarMenuClosed { .. } => {
                let owned: Vec<CanonicalId> = self
                    .views
                    .keys()
                    .filter(|canonical| &canonical.extension_id == extension_id)
                    .cloned()
                    .collect();
                for canonical in owned {
                    self.refresh_view(&canonical, changes);
                }
            }
            _ => {
                let owned: Vec<CanonicalId> = self
                    .views
                    .keys()
                    .filter(|canonical| &canonical.extension_id == extension_id)
                    .cloned()
                    .collect();
                for canonical in owned {
                    self.refresh_view(&canonical, changes);
                }
            }
        }
    }

    // A rejected view tree is otherwise indistinguishable from "nothing changed": the extension
    // keeps whatever it last rendered successfully, with no error surfaced anywhere. That's
    // fine for a transient host hiccup, but silent for a genuine authoring mistake -- a
    // container missing a required field, an invalid style combination -- which then reads as
    // the view being permanently stuck rather than what it actually is.
    fn refresh_view(&mut self, canonical: &CanonicalId, changes: &mut ExtensionChanges) {
        match self.host.render_view(canonical) {
            Ok(Some(view)) => {
                self.views.insert(canonical.clone(), view);
                changes.invalidated_views.push(canonical.clone());
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    canonical = %canonical,
                    %error,
                    "extension view render failed; keeping the last successfully rendered frame"
                );
            }
        }
    }

    pub fn dispatch_to_extension(
        &mut self,
        extension_id: &ExtensionId,
        event: &ExtensionEvent,
    ) -> ExtensionChanges {
        let mut changes = ExtensionChanges::default();
        if let Ok(result) = self.host.dispatch_event(extension_id, event) {
            for authorized in result.accepted {
                self.apply_effect(extension_id, authorized.kind(), &mut changes);
            }
            self.refresh_event_views(event, extension_id, &mut changes);
        }
        changes
    }

    fn apply_effect(
        &mut self,
        extension_id: &ExtensionId,
        effect_kind: &crate::AuthorizedHostOperationKind,
        changes: &mut ExtensionChanges,
    ) {
        match effect_kind {
            crate::AuthorizedHostOperationKind::NonHttp(effect) => match effect {
                HostOperation::ShowNotification { .. }
                | HostOperation::InvokeAction { .. }
                | HostOperation::SetWallpaper { .. }
                | HostOperation::SetThemeSource { .. }
                | HostOperation::ClipboardWrite { .. }
                | HostOperation::LocationRead => {
                    if let Ok(authorized) = crate::AuthorizedHostOperation::non_http(effect.clone())
                    {
                        changes.effects.push((extension_id.clone(), authorized));
                    }
                }
                HostOperation::HttpRequest { .. } => {}
            },
            crate::AuthorizedHostOperationKind::HttpRequest(request) => {
                changes.effects.push((
                    extension_id.clone(),
                    crate::AuthorizedHostOperation::http_request(
                        request.request_id().to_owned(),
                        crate::CanonicalHttpTarget::parse(request.url().as_str(), "GET")
                            .expect("AuthorizedHttpRequest contains a valid target"),
                    ),
                ));
            }
        }
    }
}

pub struct ExtensionEngine<R = crate::WasmRuntime> {
    paths: CatalogPaths,
    catalog: ExtensionCatalog,
    catalog_mtime: Option<std::time::SystemTime>,
    active_sources: BTreeMap<ExtensionId, ActiveSource>,
    active_dev_overrides: BTreeMap<ExtensionId, ActiveSource>,
    dev_session_sequences: HashMap<String, u64>,
    session: ExtensionSession<R>,
    script_runtime: crate::script::ScriptRuntime,
    generation: ExtensionGeneration,
    diagnostics: Vec<String>,
    replaceable_events: HashMap<std::mem::Discriminant<ReplaceableEvent>, ReplaceableEvent>,
    pending_reconcile: Option<Vec<ContributionInstance>>,
}

impl<R: ExtensionRuntime> ExtensionEngine<R> {
    pub fn new(runtime: R, paths: CatalogPaths) -> Result<Self, String> {
        Self::new_with_clock(runtime, paths, Arc::new(SystemMonotonicClock))
    }

    pub fn new_with_clock(
        runtime: R,
        paths: CatalogPaths,
        clock: Arc<dyn MonotonicClock>,
    ) -> Result<Self, String> {
        let shilpo_version =
            Version::parse(CURRENT_SHILPO_VERSION).expect("Shilpo version is valid semver");
        let catalog = ExtensionCatalog::open(paths.clone(), shilpo_version);
        let catalog_mtime = catalog_mtime(&paths.data_dir);
        let mut session = ExtensionSession::new(runtime);
        session.host = session.host.with_clock(clock);
        let grant_catalog = catalog.clone();
        session
            .host
            .runtime_mut()
            .set_grant_checker(Arc::new(move |extension_id, scope| {
                let Some(purpose) = scope.strip_prefix("secrets:") else {
                    return false;
                };
                let Ok(grants) = grant_catalog.load_grants(extension_id) else {
                    return false;
                };
                grants.granted_capabilities.iter().any(|capability| {
                    matches!(
                        capability,
                        Capability::Secrets { purposes }
                            if purposes.iter().any(|declared| declared.as_str() == purpose)
                    )
                })
            }));

        let script_runtime = crate::script::ScriptRuntime::new(paths.clone());

        let mut engine = Self {
            paths,
            catalog,
            catalog_mtime,
            active_sources: BTreeMap::new(),
            active_dev_overrides: BTreeMap::new(),
            dev_session_sequences: HashMap::new(),
            session,
            script_runtime,
            generation: ExtensionGeneration(0),
            diagnostics: Vec::new(),
            replaceable_events: HashMap::new(),
            pending_reconcile: None,
        };

        engine.reload_all()?;
        Ok(engine)
    }

    pub fn generation(&self) -> ExtensionGeneration {
        self.generation
    }

    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    pub fn handle_command(&mut self, command: ExtensionCommand) -> Option<ExtensionUpdate> {
        let command_kind = format!("{:?}", std::mem::discriminant(&command));
        let _span = tracing::info_span!(
            target: "shilpo_profile",
            "extension_command",
            command = %command_kind,
            host_generation = 0u64,
            engine_generation = self.generation.0,
            outcome = "failure",
        );
        let _enter = _span.enter();
        let mut result = match command {
            ExtensionCommand::Lifecycle { expected, event } => {
                if expected != self.generation {
                    return None;
                }
                let changes = self.session.dispatch(&event);
                Some(ExtensionUpdate {
                    host_generation: HostGeneration(0),
                    generation: self.generation,
                    snapshot: (!changes.invalidated_views.is_empty())
                        .then(|| self.build_snapshot(false)),
                    effects: changes.effects,
                    invalidated_views: changes.invalidated_views,
                    circuit_notices: Vec::new(),
                })
            }
            ExtensionCommand::Input {
                expected,
                contribution,
                instance_id,
                event_id,
                value,
            } => {
                if expected != self.generation {
                    return None;
                }
                let event = ExtensionEvent::Input {
                    contribution_id: contribution.to_string(),
                    instance_id,
                    event_id,
                    value,
                };
                let changes = self
                    .session
                    .dispatch_to_extension(&contribution.extension_id, &event);
                Some(ExtensionUpdate {
                    host_generation: HostGeneration(0),
                    generation: self.generation,
                    snapshot: (!changes.invalidated_views.is_empty())
                        .then(|| self.build_snapshot(false)),
                    effects: changes.effects,
                    invalidated_views: changes.invalidated_views,
                    circuit_notices: Vec::new(),
                })
            }
            ExtensionCommand::Response {
                expected,
                extension_id,
                event,
            } => {
                if expected != self.generation {
                    return None;
                }
                let changes = self.session.dispatch_to_extension(&extension_id, &event);
                Some(ExtensionUpdate {
                    host_generation: HostGeneration(0),
                    generation: self.generation,
                    snapshot: (!changes.invalidated_views.is_empty())
                        .then(|| self.build_snapshot(false)),
                    effects: changes.effects,
                    invalidated_views: changes.invalidated_views,
                    circuit_notices: Vec::new(),
                })
            }
            ExtensionCommand::Replaceable(event) => {
                self.replaceable_events
                    .insert(std::mem::discriminant(&event), event.clone());
                let ext_event = match event {
                    ReplaceableEvent::Power {
                        percentage,
                        charging,
                    } => ExtensionEvent::PowerChanged {
                        percentage,
                        charging,
                    },
                    ReplaceableEvent::Network { connected } => {
                        ExtensionEvent::NetworkChanged { connected }
                    }
                    ReplaceableEvent::Media {
                        title,
                        artist,
                        playing,
                    } => ExtensionEvent::MediaChanged {
                        title,
                        artist,
                        playing,
                    },
                    ReplaceableEvent::TimerFired(name) => ExtensionEvent::TimerFired { name },
                };
                let changes = self.session.dispatch(&ext_event);
                Some(ExtensionUpdate {
                    host_generation: HostGeneration(0),
                    generation: self.generation,
                    snapshot: None,
                    effects: changes.effects,
                    invalidated_views: changes.invalidated_views,
                    circuit_notices: Vec::new(),
                })
            }
            ExtensionCommand::ReconcileInstances { expected, desired } => {
                if expected != self.generation {
                    return None;
                }
                let changes = self.reconcile_instances(desired);
                Some(ExtensionUpdate {
                    host_generation: HostGeneration(0),
                    generation: self.generation,
                    snapshot: None,
                    effects: changes.effects,
                    invalidated_views: changes.invalidated_views,
                    circuit_notices: Vec::new(),
                })
            }
            ExtensionCommand::SourcesChanged => {
                if let Err(error) = self.reload_all() {
                    self.diagnostics.push(format!("reload failed: {error}"));
                }
                let snapshot = self.build_snapshot(true);
                Some(ExtensionUpdate {
                    host_generation: HostGeneration(0),
                    generation: self.generation,
                    snapshot: Some(snapshot),
                    effects: Vec::new(),
                    invalidated_views: Vec::new(),
                    circuit_notices: Vec::new(),
                })
            }
            ExtensionCommand::DevReload {
                session_id,
                extension_id,
                canonical_root,
                artifact_path,
                build_sequence,
                ..
            } => {
                let outcome = self.handle_dev_reload(
                    session_id,
                    extension_id,
                    canonical_root,
                    artifact_path,
                    build_sequence,
                );
                outcome.update
            }
            ExtensionCommand::DevUnload {
                session_id,
                extension_id,
                ..
            } => self.handle_dev_unload(&session_id, &extension_id),
            // Unreachable via the real dispatch path: `run_extension_host` in
            // `worker/process.rs` intercepts `Search` before ever calling
            // `handle_command`, since its reply shape (`WorkerPayload::Search`) doesn't
            // fit the `Option<ExtensionUpdate>` every other command returns here. Kept
            // only because the match must stay exhaustive over `ExtensionCommand`.
            ExtensionCommand::Search { .. } => None,
            ExtensionCommand::RecordSearchTimeout { extension_id } => {
                self.session.host.record_coordinator_timeout(&extension_id);
                None
            }
            ExtensionCommand::Shutdown => {
                self.script_runtime.shutdown();
                None
            }
        };
        let circuit_notices = self
            .session
            .host
            .circuit_breaker_mut()
            .take_pending_notices();
        let visible_changed = self
            .session
            .host
            .circuit_breaker_mut()
            .take_visible_changed();
        if let Some(ref mut update) = result {
            if (visible_changed || !circuit_notices.is_empty()) && update.snapshot.is_none() {
                update.snapshot = Some(self.build_snapshot(false));
            }
            update.circuit_notices = circuit_notices;
        }
        if result.is_some() {
            _span.record("outcome", "success");
        }
        result
    }

    pub fn handle_search(
        &mut self,
        canonical: &CanonicalId,
        request: &shilpo_ext_api::bindings::shilpo::extension::types::SearchRequest,
        budget: crate::RuntimeBudget,
    ) -> Result<
        Vec<shilpo_ext_api::bindings::shilpo::extension::types::SearchCandidate>,
        super::protocol::WorkerSearchError,
    > {
        self.session
            .host
            .search(canonical, request, budget)
            .map_err(|err| match err {
                crate::adapter::HostError::NotRegistered(id) => {
                    super::protocol::WorkerSearchError::NotRegistered(id)
                }
                crate::adapter::HostError::UnknownContribution(cid) => {
                    super::protocol::WorkerSearchError::UnknownContribution(cid)
                }
                crate::adapter::HostError::Disabled(id) => {
                    super::protocol::WorkerSearchError::Disabled(id)
                }
                crate::adapter::HostError::Runtime(runtime_err) => {
                    if runtime_err.kind() == crate::RuntimeFailureKind::Timeout {
                        super::protocol::WorkerSearchError::Timeout
                    } else {
                        super::protocol::WorkerSearchError::Guest(runtime_err.message().to_string())
                    }
                }
                other => super::protocol::WorkerSearchError::Other(other.to_string()),
            })
    }

    fn reconcile_instances(&mut self, desired: Vec<ContributionInstance>) -> ExtensionChanges {
        let desired_keys: HashMap<String, ContributionInstance> = desired
            .into_iter()
            .map(|inst| (inst.id.clone(), inst))
            .collect();

        let mut changes = ExtensionChanges::default();
        let current_keys: Vec<String> = self.session.instances.keys().cloned().collect();

        for key in current_keys {
            if !desired_keys.contains_key(&key)
                && let Some(instance) = self.session.instances.get(&key)
            {
                let event = ExtensionEvent::ContributionUnmounted {
                    // Full canonical string ("extension_id/contribution_id"): `ExtensionSession::dispatch`
                    // parses this field as a `CanonicalId` to resolve which extension owns the
                    // event, so it must stay qualified here. The WASM guest is handed only the
                    // bare contribution id later, at the actual host-to-guest boundary in
                    // `convert_event_to_wit`, which strips the prefix once the target extension
                    // is already known.
                    contribution_id: instance.contribution.to_string(),
                    instance_id: Some(instance.id.clone()),
                };
                changes.merge(self.session.dispatch(&event));
            }
        }

        for (key, instance) in desired_keys {
            if let Some(existing) = self.session.instances.get_mut(&key) {
                let width_changed = (existing.width - instance.width).abs() > f32::EPSILON
                    || (existing.height - instance.height).abs() > f32::EPSILON;
                let settings_changed = existing.settings != instance.settings;
                if width_changed {
                    existing.width = instance.width;
                    existing.height = instance.height;
                }
                if settings_changed {
                    existing.settings = instance.settings.clone();
                }
                // Full canonical string -- see the comment on the unmount event above.
                let contribution_str = instance.contribution.to_string();
                let instance_id = instance.id.clone();
                let width = instance.width;
                let height = instance.height;
                let settings = instance.settings;
                if width_changed {
                    let event = ExtensionEvent::ContributionResized {
                        contribution_id: contribution_str.clone(),
                        instance_id: Some(instance_id.clone()),
                        width,
                        height,
                    };
                    changes.merge(self.session.dispatch(&event));
                }
                if settings_changed {
                    let event = ExtensionEvent::ContributionSettingsChanged {
                        contribution_id: contribution_str,
                        instance_id: Some(instance_id),
                        settings,
                    };
                    changes.merge(self.session.dispatch(&event));
                }
            } else {
                // Full canonical string -- see the comment on the unmount event above.
                let contribution_str = instance.contribution.to_string();
                let instance_id = instance.id.clone();
                let event = ExtensionEvent::ContributionMounted {
                    contribution_id: contribution_str.clone(),
                    instance_id: Some(instance_id.clone()),
                    width: instance.width,
                    height: instance.height,
                };
                changes.merge(self.session.dispatch(&event));
                // A freshly mounted contribution has never seen its own settings before,
                // so this is unconditionally "new" from the extension's point of view --
                // without this, an extension whose refresh logic only reacts to
                // `ContributionSettingsChanged` (matching how the settings-changed event
                // is documented and how existing extensions are written) never gets its
                // persisted settings until something *changes* them again later, and
                // starts out permanently stuck showing nothing but its no-data fallback.
                let settings_event = ExtensionEvent::ContributionSettingsChanged {
                    contribution_id: contribution_str,
                    instance_id: Some(instance_id),
                    settings: instance.settings,
                };
                changes.merge(self.session.dispatch(&settings_event));
            }
        }

        changes
    }

    pub fn reload_all(&mut self) -> Result<(), String> {
        self.diagnostics.clear();
        let new_sources = self.discover_sources()?;
        let catalog_changed = self.catalog_mtime != catalog_mtime(&self.paths.data_dir);

        if new_sources == self.active_sources && !catalog_changed {
            return Ok(());
        }

        let preserved_instances: Vec<ContributionInstance> =
            self.session.instances.values().cloned().collect();

        // Compile every replacement before touching the active session. A malformed or
        // incomplete source must not evict the last-valid runtime, views, instances, or
        // circuit state. Registration is deliberately kept after this validation barrier
        // because the runtime module handle is owned by the session once installed.
        let mut compiled_modules = BTreeMap::new();
        let mut preflight_failed = false;
        for (id, source) in &new_sources {
            let wasm_file = source.root.join("extension.wasm");
            let wasm_bytes = match fs::read(&wasm_file) {
                Ok(bytes) => bytes,
                Err(error) => {
                    self.diagnostics
                        .push(format!("failed to read extension WASM for '{id}': {error}"));
                    preflight_failed = true;
                    continue;
                }
            };
            match self.session.host.runtime().compile_module(&wasm_bytes) {
                Ok(module) => {
                    compiled_modules.insert(id.clone(), module);
                }
                Err(error) => {
                    self.diagnostics.push(format!(
                        "failed to compile extension module for '{id}': {error}"
                    ));
                    preflight_failed = true;
                }
            }
        }
        if !preflight_failed {
            for (id, module) in &compiled_modules {
                let source = new_sources
                    .get(id)
                    .expect("compiled module belongs to an active source");
                let runtime_budget = self.session.host.runtime_budget();
                if let Err(error) = self
                    .session
                    .host
                    .runtime_mut()
                    .validate_module_with_capabilities(
                        id,
                        module,
                        runtime_budget,
                        source.manifest.capabilities.clone(),
                        source.grants.clone(),
                    )
                {
                    self.diagnostics.push(format!(
                        "failed to validate extension module for '{id}': {error}"
                    ));
                    preflight_failed = true;
                }
            }
        }
        if preflight_failed {
            return Ok(());
        }

        self.session.clear();
        self.active_sources = new_sources;
        self.catalog_mtime = catalog_mtime(&self.paths.data_dir);

        for (id, source) in &self.active_sources {
            let module = compiled_modules
                .remove(id)
                .expect("preflight compiled every active source");
            if let Err(error) =
                self.session
                    .register(source.manifest.clone(), module, source.grants.clone())
            {
                self.diagnostics
                    .push(format!("failed to register extension '{id}': {error}"));
            }
        }

        self.generation = self.generation.next();
        self.session.dispatch(&ExtensionEvent::ShellStarted);

        for event in self.replaceable_events.values() {
            let ext_event = match event {
                ReplaceableEvent::Power {
                    percentage,
                    charging,
                } => ExtensionEvent::PowerChanged {
                    percentage: *percentage,
                    charging: *charging,
                },
                ReplaceableEvent::Network { connected } => ExtensionEvent::NetworkChanged {
                    connected: *connected,
                },
                ReplaceableEvent::Media {
                    title,
                    artist,
                    playing,
                } => ExtensionEvent::MediaChanged {
                    title: title.clone(),
                    artist: artist.clone(),
                    playing: *playing,
                },
                ReplaceableEvent::TimerFired(name) => {
                    ExtensionEvent::TimerFired { name: name.clone() }
                }
            };
            self.session.dispatch(&ext_event);
        }

        self.pending_reconcile = Some(preserved_instances);
        let active_wasm_ids: Vec<ExtensionId> = self.active_sources.keys().cloned().collect();
        self.script_runtime.reconcile(&active_wasm_ids);
        Ok(())
    }

    pub fn tick(&mut self) -> Option<ExtensionUpdate> {
        let circuit_transitioned = self.session.host.circuit_breaker_mut().advance_clock();
        let circuit_notices = self
            .session
            .host
            .circuit_breaker_mut()
            .take_pending_notices();
        let visible_changed = self
            .session
            .host
            .circuit_breaker_mut()
            .take_visible_changed();
        let script_changed = self.script_runtime.tick();

        if !circuit_transitioned
            && !visible_changed
            && !script_changed
            && circuit_notices.is_empty()
        {
            return None;
        }

        self.generation = self.generation.next();
        Some(ExtensionUpdate {
            host_generation: HostGeneration(0),
            generation: self.generation,
            snapshot: Some(self.build_snapshot(false)),
            effects: Vec::new(),
            invalidated_views: if script_changed {
                self.script_runtime.views().into_keys().collect()
            } else {
                Vec::new()
            },
            circuit_notices,
        })
    }

    pub fn tick_scripts(&mut self) -> Option<ExtensionUpdate> {
        self.tick()
    }

    pub fn next_tick_deadline(&self) -> Option<Duration> {
        self.session.host.circuit_breaker().next_retry_deadline()
    }

    fn discover_sources(&mut self) -> Result<BTreeMap<ExtensionId, ActiveSource>, String> {
        let mut map = BTreeMap::new();

        let installed = self
            .catalog
            .installed()
            .map_err(|error| error.to_string())?;

        for cat_ext in installed {
            let root = cat_ext.package_dir.clone();
            let manifest = cat_ext.manifest;
            let grants = cat_ext.grants.granted_capabilities;

            let shilpo_version =
                Version::parse(CURRENT_SHILPO_VERSION).expect("Shilpo version is valid semver");
            if manifest.min_shilpo_version > shilpo_version {
                self.diagnostics.push(format!(
                    "extension '{}' requires Shilpo >= {}, but current is {}",
                    manifest.id, manifest.min_shilpo_version, shilpo_version
                ));
                continue;
            }

            let fingerprint = compute_fingerprint(&root, &manifest);
            map.insert(
                manifest.id.clone(),
                ActiveSource {
                    manifest,
                    root,
                    grants,
                    fingerprint,
                },
            );
        }

        let state_dir = crate::default_extension_state_dir();
        let (dev_registrations, diags) = crate::development_registrations(&state_dir);
        self.diagnostics.extend(diags);

        for reg in dev_registrations {
            let id = reg.id;
            let root = reg.path;
            let manifest_path = root.join("extension.toml");
            let toml_str = match fs::read_to_string(&manifest_path) {
                Ok(s) => s,
                Err(error) => {
                    self.diagnostics.push(format!(
                        "failed to read dev manifest at {}: {error}",
                        manifest_path.display()
                    ));
                    continue;
                }
            };
            let manifest = match ExtensionManifest::from_toml(&toml_str) {
                Ok(m) => m,
                Err(error) => {
                    self.diagnostics.push(format!(
                        "invalid dev manifest at {}: {error}",
                        manifest_path.display()
                    ));
                    continue;
                }
            };

            let grants = self
                .catalog
                .load_grants(&manifest.id)
                .map(|g| g.granted_capabilities)
                .unwrap_or_default();

            let fingerprint = compute_fingerprint(&root, &manifest);
            map.insert(
                id,
                ActiveSource {
                    manifest,
                    root,
                    grants,
                    fingerprint,
                },
            );
        }

        Ok(map)
    }

    pub fn build_snapshot(&mut self, catalog_changed: bool) -> ExtensionSnapshot {
        if let Some(desired) = self.pending_reconcile.take() {
            self.reconcile_instances(desired);
        }

        let mut descriptors = Vec::new();
        let mut settings_schemas = BTreeMap::new();
        let mut prevalidated_asset_roots = BTreeMap::new();

        for (ext_id, source) in &self.active_sources {
            prevalidated_asset_roots.insert(ext_id.clone(), source.root.clone());
            let m = &source.manifest;

            for contrib in &m.contributions.bar_widgets {
                descriptors.push(ContributionDescriptor {
                    id: CanonicalId::new(ext_id.clone(), contrib.id.clone()),
                    extension_name: m.name.clone(),
                    name: contrib.name.clone(),
                    surface: ContributionSurface::Bar,
                    runtime_kind: ExtensionRuntimeKind::Wasm,
                    settings_schema: None,
                    default_size: None,
                    minimum_size: None,
                    bar_widget: None,
                    action: None,
                    default_binding: None,
                    wallpaper_modes: None,
                    wallpaper_targets: None,
                    search_modes: None,
                });
            }
            for contrib in &m.contributions.bar_menus {
                descriptors.push(ContributionDescriptor {
                    id: CanonicalId::new(ext_id.clone(), contrib.id.clone()),
                    extension_name: m.name.clone(),
                    name: contrib.name.clone(),
                    surface: ContributionSurface::BarMenu,
                    runtime_kind: ExtensionRuntimeKind::Wasm,
                    settings_schema: None,
                    default_size: None,
                    minimum_size: None,
                    bar_widget: Some(CanonicalId::new(ext_id.clone(), contrib.bar_widget.clone())),
                    action: None,
                    default_binding: None,
                    wallpaper_modes: None,
                    wallpaper_targets: None,
                    search_modes: None,
                });
            }
            for contrib in &m.contributions.desktop_widgets {
                let default_size = match (contrib.default_width, contrib.default_height) {
                    (Some(w), Some(h)) => Some((w, h)),
                    _ => None,
                };
                let minimum_size = match (contrib.min_width, contrib.min_height) {
                    (Some(w), Some(h)) => Some((w, h)),
                    _ => None,
                };
                descriptors.push(ContributionDescriptor {
                    id: CanonicalId::new(ext_id.clone(), contrib.id.clone()),
                    extension_name: m.name.clone(),
                    name: contrib.name.clone(),
                    surface: ContributionSurface::Desktop,
                    runtime_kind: ExtensionRuntimeKind::Wasm,
                    settings_schema: None,
                    default_size,
                    minimum_size,
                    bar_widget: None,
                    action: None,
                    default_binding: None,
                    wallpaper_modes: None,
                    wallpaper_targets: None,
                    search_modes: None,
                });
            }
            for contrib in &m.contributions.settings_pages {
                let canonical = CanonicalId::new(ext_id.clone(), contrib.id.clone());
                let schema_path = source.root.join(&contrib.schema);
                if let Ok(schema_str) = fs::read_to_string(&schema_path)
                    && let Ok(json_val) = serde_json::from_str(&schema_str)
                {
                    settings_schemas.insert(canonical.clone(), json_val);
                }
                descriptors.push(ContributionDescriptor {
                    id: canonical,
                    extension_name: m.name.clone(),
                    name: contrib.name.clone(),
                    surface: ContributionSurface::Settings,
                    runtime_kind: ExtensionRuntimeKind::Wasm,
                    settings_schema: Some(contrib.schema.clone()),
                    default_size: None,
                    minimum_size: None,
                    bar_widget: None,
                    action: None,
                    default_binding: None,
                    wallpaper_modes: None,
                    wallpaper_targets: None,
                    search_modes: None,
                });
            }
            for contrib in &m.contributions.side_panels {
                descriptors.push(ContributionDescriptor {
                    id: CanonicalId::new(ext_id.clone(), contrib.id.clone()),
                    extension_name: m.name.clone(),
                    name: contrib.name.clone(),
                    surface: ContributionSurface::SidePanel,
                    runtime_kind: ExtensionRuntimeKind::Wasm,
                    settings_schema: None,
                    default_size: None,
                    minimum_size: None,
                    bar_widget: None,
                    action: None,
                    default_binding: None,
                    wallpaper_modes: None,
                    wallpaper_targets: None,
                    search_modes: None,
                });
            }
            for contrib in &m.contributions.actions {
                descriptors.push(ContributionDescriptor {
                    id: CanonicalId::new(ext_id.clone(), contrib.id.clone()),
                    extension_name: m.name.clone(),
                    name: contrib.name.clone(),
                    surface: ContributionSurface::Action,
                    runtime_kind: ExtensionRuntimeKind::Wasm,
                    settings_schema: None,
                    default_size: None,
                    minimum_size: None,
                    bar_widget: None,
                    action: None,
                    default_binding: None,
                    wallpaper_modes: None,
                    wallpaper_targets: None,
                    search_modes: None,
                });
            }
            for contrib in &m.contributions.keyboard_shortcuts {
                descriptors.push(ContributionDescriptor {
                    id: CanonicalId::new(ext_id.clone(), contrib.id.clone()),
                    extension_name: m.name.clone(),
                    name: contrib.name.clone(),
                    surface: ContributionSurface::Shortcut,
                    runtime_kind: ExtensionRuntimeKind::Wasm,
                    settings_schema: None,
                    default_size: None,
                    minimum_size: None,
                    bar_widget: None,
                    action: Some(CanonicalId::new(ext_id.clone(), contrib.action.clone())),
                    default_binding: contrib.default_binding.clone(),
                    wallpaper_modes: None,
                    wallpaper_targets: None,
                    search_modes: None,
                });
            }
            for contrib in &m.contributions.background_tasks {
                descriptors.push(ContributionDescriptor {
                    id: CanonicalId::new(ext_id.clone(), contrib.id.clone()),
                    extension_name: m.name.clone(),
                    name: contrib.name.clone(),
                    surface: ContributionSurface::Background,
                    runtime_kind: ExtensionRuntimeKind::Wasm,
                    settings_schema: None,
                    default_size: None,
                    minimum_size: None,
                    bar_widget: None,
                    action: None,
                    default_binding: None,
                    wallpaper_modes: None,
                    wallpaper_targets: None,
                    search_modes: None,
                });
            }
            for contrib in &m.contributions.wallpaper_providers {
                descriptors.push(ContributionDescriptor {
                    id: CanonicalId::new(ext_id.clone(), contrib.id.clone()),
                    extension_name: m.name.clone(),
                    name: contrib.name.clone(),
                    surface: ContributionSurface::Wallpaper,
                    runtime_kind: ExtensionRuntimeKind::Wasm,
                    settings_schema: None,
                    default_size: None,
                    minimum_size: None,
                    bar_widget: None,
                    action: None,
                    default_binding: None,
                    wallpaper_modes: Some(contrib.modes.clone()),
                    wallpaper_targets: Some(contrib.targets.clone()),
                    search_modes: None,
                });
            }
            for contrib in &m.contributions.search_providers {
                descriptors.push(ContributionDescriptor {
                    id: CanonicalId::new(ext_id.clone(), contrib.id.clone()),
                    extension_name: m.name.clone(),
                    name: contrib.name.clone(),
                    surface: ContributionSurface::SearchProvider,
                    runtime_kind: ExtensionRuntimeKind::Wasm,
                    settings_schema: None,
                    default_size: None,
                    minimum_size: None,
                    bar_widget: None,
                    action: None,
                    default_binding: None,
                    wallpaper_modes: None,
                    wallpaper_targets: None,
                    search_modes: Some(contrib.modes.clone()),
                });
            }
        }

        for desc in self.script_runtime.descriptors() {
            descriptors.push(desc);
        }

        let mut views: BTreeMap<CanonicalId, ViewTree> =
            self.session.views.clone().into_iter().collect();
        for (k, v) in self.script_runtime.views() {
            views.insert(k, v);
        }

        for (id, root) in self.script_runtime.asset_roots() {
            prevalidated_asset_roots.insert(id, root);
        }

        let mut all_diagnostics = self.diagnostics.clone();
        all_diagnostics.extend(self.script_runtime.diagnostics());

        ExtensionSnapshot {
            generation: self.generation,
            descriptors: Arc::from(descriptors),
            views: Arc::new(views),
            diagnostics: Arc::from(all_diagnostics),
            catalog_changed_at: if catalog_changed {
                Some(self.generation)
            } else {
                None
            },
            settings_schemas: Arc::new(settings_schemas),
            prevalidated_asset_roots: Arc::new(prevalidated_asset_roots),
            script_extensions: Arc::from(self.script_runtime.statuses()),
            wasm_extensions: Arc::from(
                self.active_sources
                    .keys()
                    .map(|id| self.session.host.circuit_breaker().status(id))
                    .collect::<Vec<_>>(),
            ),
            dev_overrides: Arc::from(
                self.active_dev_overrides
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>(),
            ),
        }
    }

    pub fn handle_dev_reload(
        &mut self,
        session_id: String,
        extension_id: ExtensionId,
        canonical_root: PathBuf,
        artifact_path: PathBuf,
        build_sequence: u64,
    ) -> DevReloadOutcome {
        if let Some(&last_seq) = self.dev_session_sequences.get(&session_id)
            && build_sequence <= last_seq
        {
            return DevReloadOutcome::rejected(
                session_id,
                build_sequence,
                self.generation,
                "STALE_BUILD_SEQUENCE",
                format!("stale build sequence {build_sequence} <= last accepted {last_seq}"),
            );
        }

        if !canonical_root.is_dir() {
            return DevReloadOutcome::rejected(
                session_id,
                build_sequence,
                self.generation,
                "INVALID_ROOT",
                format!(
                    "source root '{}' is not a directory",
                    canonical_root.display()
                ),
            );
        }

        let canonical_artifact = match artifact_path.canonicalize() {
            Ok(p) => p,
            Err(e) => {
                return DevReloadOutcome::rejected(
                    session_id,
                    build_sequence,
                    self.generation,
                    "ARTIFACT_NOT_FOUND",
                    format!("artifact '{}' not found: {e}", artifact_path.display()),
                );
            }
        };

        if !canonical_artifact.starts_with(&canonical_root) {
            return DevReloadOutcome::rejected(
                session_id,
                build_sequence,
                self.generation,
                "ARTIFACT_OUTSIDE_ROOT",
                format!(
                    "artifact '{}' is outside source root '{}'",
                    canonical_artifact.display(),
                    canonical_root.display()
                ),
            );
        }

        if !canonical_artifact.is_file() {
            return DevReloadOutcome::rejected(
                session_id,
                build_sequence,
                self.generation,
                "ARTIFACT_NOT_FILE",
                format!(
                    "artifact '{}' is not a regular file",
                    canonical_artifact.display()
                ),
            );
        }

        let manifest_path = canonical_root.join("extension.toml");
        let toml_str = match fs::read_to_string(&manifest_path) {
            Ok(s) => s,
            Err(e) => {
                return DevReloadOutcome::rejected(
                    session_id,
                    build_sequence,
                    self.generation,
                    "MANIFEST_NOT_FOUND",
                    format!(
                        "failed to read manifest at '{}': {e}",
                        manifest_path.display()
                    ),
                );
            }
        };

        let manifest = match ExtensionManifest::from_toml(&toml_str) {
            Ok(m) => m,
            Err(e) => {
                return DevReloadOutcome::rejected(
                    session_id,
                    build_sequence,
                    self.generation,
                    "INVALID_MANIFEST",
                    format!("invalid manifest at '{}': {e}", manifest_path.display()),
                );
            }
        };

        if manifest.id != extension_id {
            return DevReloadOutcome::rejected(
                session_id,
                build_sequence,
                self.generation,
                "ID_MISMATCH",
                format!(
                    "manifest declares ID '{}' but session bound to '{}'",
                    manifest.id, extension_id
                ),
            );
        }

        let shilpo_version =
            Version::parse(CURRENT_SHILPO_VERSION).expect("Shilpo version is valid semver");
        if manifest.min_shilpo_version > shilpo_version {
            return DevReloadOutcome::rejected(
                session_id,
                build_sequence,
                self.generation,
                "UNSUPPORTED_SHILPO_VERSION",
                format!(
                    "extension '{}' requires Shilpo >= {}, current is {}",
                    extension_id, manifest.min_shilpo_version, shilpo_version
                ),
            );
        }

        let wasm_bytes = match fs::read(&canonical_artifact) {
            Ok(b) => b,
            Err(e) => {
                return DevReloadOutcome::rejected(
                    session_id,
                    build_sequence,
                    self.generation,
                    "READ_FAILED",
                    format!("failed to read WASM artifact: {e}"),
                );
            }
        };

        let module = match self.session.host.runtime().compile_module(&wasm_bytes) {
            Ok(m) => m,
            Err(e) => {
                return DevReloadOutcome::rejected(
                    session_id,
                    build_sequence,
                    self.generation,
                    "COMPILE_ERROR",
                    format!("failed to compile WASM component: {e}"),
                );
            }
        };

        let grants = self
            .catalog
            .load_grants(&extension_id)
            .map(|g| g.granted_capabilities)
            .unwrap_or_else(|_| manifest.capabilities.clone());

        let runtime_budget = self.session.host.runtime_budget();
        if let Err(e) = self
            .session
            .host
            .runtime_mut()
            .validate_module_with_capabilities(
                &extension_id,
                &module,
                runtime_budget,
                manifest.capabilities.clone(),
                grants.clone(),
            )
        {
            return DevReloadOutcome::rejected(
                session_id,
                build_sequence,
                self.generation,
                "VALIDATION_ERROR",
                format!("WASM validation failed: {e}"),
            );
        }

        // Candidate valid! Perform atomic swap.
        let is_registered = self.session.manifests.contains_key(&extension_id);
        if is_registered && !self.active_dev_overrides.contains_key(&extension_id) {
            return DevReloadOutcome::rejected(
                session_id,
                build_sequence,
                self.generation,
                "NON_DEV_ACTIVATION",
                format!(
                    "extension '{}' is already active outside a development session",
                    extension_id
                ),
            );
        }
        if is_registered {
            if let Err(e) = self
                .session
                .host
                .replace(manifest.clone(), module, grants.clone())
            {
                return DevReloadOutcome::rejected(
                    session_id,
                    build_sequence,
                    self.generation,
                    "REPLACE_FAILED",
                    format!("failed to replace extension in runtime: {e}"),
                );
            }
        } else {
            if let Err(e) = self
                .session
                .host
                .register(manifest.clone(), module, grants.clone())
            {
                return DevReloadOutcome::rejected(
                    session_id,
                    build_sequence,
                    self.generation,
                    "REGISTER_FAILED",
                    format!("failed to register extension in runtime: {e}"),
                );
            }
            self.session.runtime_ids.push(extension_id.clone());
        }

        self.session
            .manifests
            .insert(extension_id.clone(), manifest.clone());

        let fingerprint = compute_fingerprint(&canonical_root, &manifest);
        let active_source = ActiveSource {
            manifest: manifest.clone(),
            root: canonical_root.clone(),
            grants,
            fingerprint,
        };
        self.active_sources
            .insert(extension_id.clone(), active_source.clone());
        self.active_dev_overrides
            .insert(extension_id.clone(), active_source);

        let mut invalidated = Vec::new();
        self.session.views.retain(|k, _| {
            if k.extension_id == extension_id {
                invalidated.push(k.clone());
                false
            } else {
                true
            }
        });

        for contrib in &manifest.contributions.bar_widgets {
            let canonical = CanonicalId::new(extension_id.clone(), contrib.id.clone());
            if let Ok(Some(view)) = self.session.host.render_view(&canonical) {
                self.session.views.insert(canonical.clone(), view);
                invalidated.push(canonical);
            }
        }
        for contrib in &manifest.contributions.bar_menus {
            let canonical = CanonicalId::new(extension_id.clone(), contrib.id.clone());
            if let Ok(Some(view)) = self.session.host.render_view(&canonical) {
                self.session.views.insert(canonical.clone(), view);
                invalidated.push(canonical);
            }
        }
        for contrib in &manifest.contributions.desktop_widgets {
            let canonical = CanonicalId::new(extension_id.clone(), contrib.id.clone());
            if let Ok(Some(view)) = self.session.host.render_view(&canonical) {
                self.session.views.insert(canonical.clone(), view);
                invalidated.push(canonical);
            }
        }

        self.dev_session_sequences
            .insert(session_id.clone(), build_sequence);
        self.generation = self.generation.next();

        self.session
            .dispatch_to_extension(&extension_id, &ExtensionEvent::ShellStarted);

        for event in self.replaceable_events.values() {
            let ext_event = match event {
                ReplaceableEvent::Power {
                    percentage,
                    charging,
                } => ExtensionEvent::PowerChanged {
                    percentage: *percentage,
                    charging: *charging,
                },
                ReplaceableEvent::Network { connected } => ExtensionEvent::NetworkChanged {
                    connected: *connected,
                },
                ReplaceableEvent::Media {
                    title,
                    artist,
                    playing,
                } => ExtensionEvent::MediaChanged {
                    title: title.clone(),
                    artist: artist.clone(),
                    playing: *playing,
                },
                ReplaceableEvent::TimerFired(name) => {
                    ExtensionEvent::TimerFired { name: name.clone() }
                }
            };
            self.session
                .dispatch_to_extension(&extension_id, &ext_event);
        }

        let snapshot = self.build_snapshot(false);
        let update = ExtensionUpdate {
            host_generation: HostGeneration(0),
            generation: self.generation,
            snapshot: Some(snapshot),
            effects: Vec::new(),
            invalidated_views: invalidated,
            circuit_notices: Vec::new(),
        };

        DevReloadOutcome::applied(
            session_id,
            build_sequence,
            self.generation,
            "Extension reloaded successfully",
            Some(update),
        )
    }

    pub fn handle_dev_unload(
        &mut self,
        session_id: &str,
        extension_id: &ExtensionId,
    ) -> Option<ExtensionUpdate> {
        self.dev_session_sequences.remove(session_id);
        self.active_dev_overrides.remove(extension_id)?;

        let mut invalidated = Vec::new();
        self.session.views.retain(|k, _| {
            if k.extension_id == *extension_id {
                invalidated.push(k.clone());
                false
            } else {
                true
            }
        });

        // If it was also in installed catalog, reload catalog version
        let catalog_sources = self.discover_sources().unwrap_or_default();
        if let Some(cat_source) = catalog_sources.get(extension_id) {
            let wasm_file = cat_source.root.join("extension.wasm");
            if let Ok(bytes) = fs::read(&wasm_file)
                && let Ok(module) = self.session.host.runtime().compile_module(&bytes)
            {
                let _ = self.session.host.replace(
                    cat_source.manifest.clone(),
                    module,
                    cat_source.grants.clone(),
                );
                self.session
                    .manifests
                    .insert(extension_id.clone(), cat_source.manifest.clone());
                self.active_sources
                    .insert(extension_id.clone(), cat_source.clone());

                for contrib in &cat_source.manifest.contributions.bar_widgets {
                    let canonical = CanonicalId::new(extension_id.clone(), contrib.id.clone());
                    if let Ok(Some(view)) = self.session.host.render_view(&canonical) {
                        self.session.views.insert(canonical.clone(), view);
                        invalidated.push(canonical);
                    }
                }
                for contrib in &cat_source.manifest.contributions.bar_menus {
                    let canonical = CanonicalId::new(extension_id.clone(), contrib.id.clone());
                    if let Ok(Some(view)) = self.session.host.render_view(&canonical) {
                        self.session.views.insert(canonical.clone(), view);
                        invalidated.push(canonical);
                    }
                }
                for contrib in &cat_source.manifest.contributions.desktop_widgets {
                    let canonical = CanonicalId::new(extension_id.clone(), contrib.id.clone());
                    if let Ok(Some(view)) = self.session.host.render_view(&canonical) {
                        self.session.views.insert(canonical.clone(), view);
                        invalidated.push(canonical);
                    }
                }
            }
        } else {
            let _ = self.session.host.unregister(extension_id);
            self.session.manifests.remove(extension_id);
            self.session.runtime_ids.retain(|id| id != extension_id);
            self.active_sources.remove(extension_id);
        }

        self.generation = self.generation.next();
        let snapshot = self.build_snapshot(false);
        Some(ExtensionUpdate {
            host_generation: HostGeneration(0),
            generation: self.generation,
            snapshot: Some(snapshot),
            effects: Vec::new(),
            invalidated_views: invalidated,
            circuit_notices: Vec::new(),
        })
    }
}

fn catalog_mtime(path: &Path) -> Option<std::time::SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn compute_fingerprint(root: &Path, manifest: &ExtensionManifest) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    manifest.id.hash(&mut hasher);
    manifest.version.hash(&mut hasher);

    let wasm_file = root.join("extension.wasm");
    if let Ok(meta) = fs::metadata(&wasm_file) {
        if let Ok(modified) = meta.modified()
            && let Ok(duration) = modified.duration_since(UNIX_EPOCH)
        {
            duration.as_nanos().hash(&mut hasher);
        }
        meta.len().hash(&mut hasher);
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        time::Instant,
    };

    use shilpo_ext_api::{BarMenuCloseReason, HostOperation, ImageNode, TextNode, ViewNode};

    use super::*;
    use crate::{
        CircuitStateKind, DiagnosticCode, FakeMonotonicClock, GuestExtension, InMemoryRuntime,
    };

    struct TestGuest;
    impl GuestExtension for TestGuest {
        fn on_event(&mut self, event: &ExtensionEvent) -> Vec<HostOperation> {
            match event {
                ExtensionEvent::ShellStarted => vec![HostOperation::ShowNotification {
                    title: "Hello".into(),
                    body: "World".into(),
                    icon: None,
                }],
                _ => Vec::new(),
            }
        }

        fn view(&self, _contribution_id: &str) -> Option<ViewTree> {
            None
        }
    }

    #[test]
    fn test_extension_session_basic_lifecycle() {
        let runtime = InMemoryRuntime::new();
        let manifest = ExtensionManifest::from_toml(
            r#"
            id = "io.github.test.sample"
            name = "Sample"
            version = "1.0.0"
            schema_version = 1
            api_version = "0.1.0"
            min_shilpo_version = "0.1.0"

            [[capabilities]]
            kind = "notifications:show"
            "#,
        )
        .unwrap();

        let mut session = ExtensionSession::new(runtime);
        session
            .register(
                manifest.clone(),
                Box::new(TestGuest),
                manifest.capabilities.clone(),
            )
            .unwrap();

        let changes = session.dispatch(&ExtensionEvent::ShellStarted);
        assert_eq!(changes.effects.len(), 1);
        assert_eq!(changes.effects[0].0.as_str(), "io.github.test.sample");
    }

    struct MenuGuest;
    impl GuestExtension for MenuGuest {
        fn on_event(&mut self, _event: &ExtensionEvent) -> Vec<HostOperation> {
            Vec::new()
        }

        fn view(&self, contribution_id: &str) -> Option<ViewTree> {
            if contribution_id == "weather-menu" {
                Some(ViewTree::new(shilpo_ext_api::ViewNode::Text(
                    shilpo_ext_api::TextNode {
                        content: "Weather Menu".into(),
                        ..Default::default()
                    },
                )))
            } else {
                None
            }
        }
    }

    fn menu_manifest(id: &str) -> ExtensionManifest {
        ExtensionManifest::from_toml(&format!(
            r#"
            id = "{id}"
            name = "Weather"
            version = "0.1.0"
            schema_version = 1
            api_version = "0.1.0"
            min_shilpo_version = "0.1.0"

            [[contributions.bar_widgets]]
            id = "weather"
            name = "Weather Widget"

            [[contributions.bar_menus]]
            id = "weather-menu"
            name = "Weather Details Menu"
            bar_widget = "weather"
            "#
        ))
        .unwrap()
    }

    #[test]
    fn test_bar_menu_descriptor_discovery_and_view_rendering() {
        let runtime = InMemoryRuntime::new();
        let manifest = ExtensionManifest::from_toml(
            r#"
            id = "io.github.test.weather"
            name = "Weather"
            version = "1.0.0"
            schema_version = 1
            api_version = "0.1.0"
            min_shilpo_version = "0.1.0"

            [[contributions.bar_widgets]]
            id = "weather"
            name = "Weather Widget"

            [[contributions.bar_menus]]
            id = "weather-menu"
            name = "Weather Details Menu"
            bar_widget = "weather"
            "#,
        )
        .unwrap();

        let mut session = ExtensionSession::new(runtime);
        session
            .register(
                manifest.clone(),
                Box::new(MenuGuest),
                manifest.capabilities.clone(),
            )
            .unwrap();

        let menu_canonical = CanonicalId::new(
            manifest.id.clone(),
            shilpo_ext_api::ContributionId::new("weather-menu").unwrap(),
        );
        let widget_canonical = CanonicalId::new(
            manifest.id.clone(),
            shilpo_ext_api::ContributionId::new("weather").unwrap(),
        );

        assert!(session.views.contains_key(&menu_canonical));

        let mut engine = ExtensionEngine {
            paths: CatalogPaths::platform_default(),
            catalog: ExtensionCatalog::open(
                CatalogPaths::platform_default(),
                semver::Version::new(0, 1, 0),
            ),
            catalog_mtime: None,
            active_sources: BTreeMap::new(),
            active_dev_overrides: BTreeMap::new(),
            dev_session_sequences: HashMap::new(),
            session,
            script_runtime: crate::script::ScriptRuntime::new(CatalogPaths::platform_default()),
            generation: ExtensionGeneration(1),
            diagnostics: Vec::new(),
            replaceable_events: HashMap::new(),
            pending_reconcile: None,
        };
        engine.active_sources.insert(
            manifest.id.clone(),
            ActiveSource {
                manifest,
                root: std::path::PathBuf::from("/tmp"),
                grants: Vec::new(),
                fingerprint: 0,
            },
        );

        let snapshot = engine.build_snapshot(false);
        let menu_desc = snapshot
            .descriptors
            .iter()
            .find(|d| d.surface == ContributionSurface::BarMenu)
            .expect("bar menu descriptor must exist");

        assert_eq!(menu_desc.id, menu_canonical);
        assert_eq!(menu_desc.bar_widget, Some(widget_canonical));
        assert_eq!(menu_desc.default_size, None);
        assert_eq!(menu_desc.minimum_size, None);
    }

    #[test]
    fn bar_menu_events_refresh_only_the_target_extension() {
        let mut session = ExtensionSession::new(InMemoryRuntime::new());
        let first = menu_manifest("io.github.test.first");
        let second = menu_manifest("io.github.test.second");
        session
            .register(
                first.clone(),
                Box::new(MenuGuest),
                first.capabilities.clone(),
            )
            .unwrap();
        session
            .register(
                second.clone(),
                Box::new(MenuGuest),
                second.capabilities.clone(),
            )
            .unwrap();

        let target = CanonicalId::new(
            first.id,
            shilpo_ext_api::ContributionId::new("weather-menu").unwrap(),
        );
        let changes = session.dispatch(&ExtensionEvent::BarMenuOpened {
            contribution_id: target.to_string(),
            instance_id: "bar:display-1:weather".into(),
        });

        // `MenuGuest::view()` only ever produces content for "weather-menu" (its "weather"
        // bar-widget contribution has none), so broadening the refresh to every view this
        // extension owns -- rather than just the menu's own -- still only actually
        // invalidates the one view that renders to anything. The real point of this test is
        // that the unrelated "second" extension is untouched.
        assert_eq!(changes.invalidated_views, vec![target]);
    }

    struct BothViewsGuest;
    impl GuestExtension for BothViewsGuest {
        fn on_event(&mut self, _event: &ExtensionEvent) -> Vec<HostOperation> {
            Vec::new()
        }

        fn view(&self, contribution_id: &str) -> Option<ViewTree> {
            Some(ViewTree::new(shilpo_ext_api::ViewNode::Text(
                shilpo_ext_api::TextNode {
                    content: contribution_id.to_owned(),
                    ..Default::default()
                },
            )))
        }
    }

    #[test]
    fn bar_menu_opened_also_refreshes_sibling_contributions_of_the_same_extension() {
        let mut session = ExtensionSession::new(InMemoryRuntime::new());
        let manifest = menu_manifest("io.github.test.weather");
        session
            .register(
                manifest.clone(),
                Box::new(BothViewsGuest),
                manifest.capabilities.clone(),
            )
            .unwrap();

        let menu = CanonicalId::new(
            manifest.id.clone(),
            shilpo_ext_api::ContributionId::new("weather-menu").unwrap(),
        );
        let widget = CanonicalId::new(
            manifest.id,
            shilpo_ext_api::ContributionId::new("weather").unwrap(),
        );
        let changes = session.dispatch(&ExtensionEvent::BarMenuOpened {
            contribution_id: menu.to_string(),
            instance_id: "bar:display-1:weather".into(),
        });

        let mut invalidated = changes.invalidated_views;
        invalidated.sort_by_key(ToString::to_string);
        let mut expected = vec![menu, widget];
        expected.sort_by_key(ToString::to_string);
        assert_eq!(invalidated, expected);
    }

    struct StatefulMenuGuest {
        state: Arc<Mutex<&'static str>>,
        events: Arc<Mutex<Vec<ExtensionEvent>>>,
    }

    impl GuestExtension for StatefulMenuGuest {
        fn on_event(&mut self, event: &ExtensionEvent) -> Vec<HostOperation> {
            self.events.lock().unwrap().push(event.clone());
            let next = match event {
                ExtensionEvent::BarMenuOpened { .. } => "opened",
                ExtensionEvent::BarMenuClosed { .. } => "invalid",
                _ => return Vec::new(),
            };
            *self.state.lock().unwrap() = next;
            Vec::new()
        }

        fn view(&self, contribution_id: &str) -> Option<ViewTree> {
            if contribution_id != "weather-menu" {
                return None;
            }
            match *self.state.lock().unwrap() {
                "invalid" => Some(ViewTree::new(ViewNode::Image(ImageNode {
                    asset_path: "../outside-sandbox.png".into(),
                    width: None,
                    height: None,
                    style: None,
                }))),
                label => Some(ViewTree::new(ViewNode::Text(TextNode {
                    content: label.into(),
                    ..Default::default()
                }))),
            }
        }
    }

    #[test]
    fn bar_menu_session_refreshes_valid_views_fails_closed_and_cleans_up_on_reload() {
        let manifest = menu_manifest("io.github.test.lifecycle");
        let menu = CanonicalId::new(
            manifest.id.clone(),
            shilpo_ext_api::ContributionId::new("weather-menu").unwrap(),
        );
        let state = Arc::new(Mutex::new("initial"));
        let events = Arc::new(Mutex::new(Vec::new()));
        let guest = || StatefulMenuGuest {
            state: state.clone(),
            events: events.clone(),
        };
        let mut session = ExtensionSession::new(InMemoryRuntime::new());
        session
            .register(
                manifest.clone(),
                Box::new(guest()),
                manifest.capabilities.clone(),
            )
            .unwrap();

        let opened = ExtensionEvent::BarMenuOpened {
            contribution_id: menu.to_string(),
            instance_id: "bar:display-1:weather".into(),
        };
        let changes = session.dispatch(&opened);
        assert_eq!(changes.invalidated_views, vec![menu.clone()]);
        assert!(matches!(
            &session.views[&menu].root,
            ViewNode::Text(node) if node.content == "opened"
        ));

        let closed = ExtensionEvent::BarMenuClosed {
            contribution_id: menu.to_string(),
            instance_id: "bar:display-1:weather".into(),
            reason: BarMenuCloseReason::Escape,
        };
        let changes = session.dispatch(&closed);
        assert!(changes.invalidated_views.is_empty());
        assert!(matches!(
            &session.views[&menu].root,
            ViewNode::Text(node) if node.content == "opened"
        ));
        assert_eq!(events.lock().unwrap().as_slice(), &[opened, closed]);

        session.clear();
        assert!(session.manifests.is_empty());
        assert!(session.runtime_ids.is_empty());
        assert!(session.views.is_empty());
        assert!(session.instances.is_empty());
        assert!(session.state.is_empty());

        *state.lock().unwrap() = "reloaded";
        session
            .register(
                manifest.clone(),
                Box::new(guest()),
                manifest.capabilities.clone(),
            )
            .unwrap();
        assert!(matches!(
            &session.views[&menu].root,
            ViewNode::Text(node) if node.content == "reloaded"
        ));
    }

    #[test]
    fn shortcut_worker_snapshot_metadata_and_isolation() {
        let manifest_toml_1 = r#"
            id = "io.github.alice.weather"
            name = "Alice Weather"
            version = "1.0.0"

            [[contributions.actions]]
            id = "toggle-action"
            name = "Toggle Weather Action"

            [[contributions.keyboard_shortcuts]]
            id = "toggle-shortcut"
            name = "Toggle Weather Shortcut"
            action = "toggle-action"
            default_binding = "Super+Shift+W"
        "#;
        let manifest_toml_2 = r#"
            id = "io.github.bob.clock"
            name = "Bob Clock"
            version = "1.0.0"

            [[contributions.actions]]
            id = "toggle-action"
            name = "Toggle Clock Action"

            [[contributions.keyboard_shortcuts]]
            id = "toggle-shortcut"
            name = "Toggle Clock Shortcut"
            action = "toggle-action"
            default_binding = "Super+Shift+C"
        "#;

        let m1 = ExtensionManifest::from_toml(manifest_toml_1).unwrap();
        let m2 = ExtensionManifest::from_toml(manifest_toml_2).unwrap();

        let temp_dir = std::env::temp_dir();
        let clock = Arc::new(FakeMonotonicClock::new(Instant::now()));
        let mut engine = ExtensionEngine::new_with_clock(
            InMemoryRuntime::new(),
            CatalogPaths::new(&temp_dir, &temp_dir),
            clock.clone(),
        )
        .unwrap();

        engine.active_sources.insert(
            m1.id.clone(),
            ActiveSource {
                manifest: m1.clone(),
                root: PathBuf::from("/tmp/alice"),
                grants: Vec::new(),
                fingerprint: 1,
            },
        );
        engine.active_sources.insert(
            m2.id.clone(),
            ActiveSource {
                manifest: m2.clone(),
                root: PathBuf::from("/tmp/bob"),
                grants: Vec::new(),
                fingerprint: 2,
            },
        );

        let snapshot = engine.build_snapshot(false);
        let shortcuts: Vec<_> = snapshot
            .descriptors
            .iter()
            .filter(|d| d.surface == ContributionSurface::Shortcut)
            .collect();

        assert_eq!(shortcuts.len(), 2);

        let alice_sc = shortcuts
            .iter()
            .find(|d| d.id.extension_id.as_str() == "io.github.alice.weather")
            .unwrap();
        assert_eq!(
            alice_sc.id.to_string(),
            "io.github.alice.weather/toggle-shortcut"
        );
        assert_eq!(
            alice_sc.action.as_ref().map(ToString::to_string),
            Some("io.github.alice.weather/toggle-action".into())
        );
        assert_eq!(alice_sc.default_binding.as_deref(), Some("Super+Shift+W"));

        let bob_sc = shortcuts
            .iter()
            .find(|d| d.id.extension_id.as_str() == "io.github.bob.clock")
            .unwrap();
        assert_eq!(bob_sc.id.to_string(), "io.github.bob.clock/toggle-shortcut");
        assert_eq!(
            bob_sc.action.as_ref().map(ToString::to_string),
            Some("io.github.bob.clock/toggle-action".into())
        );
        assert_eq!(bob_sc.default_binding.as_deref(), Some("Super+Shift+C"));

        // Unload bob: remove from active_sources
        engine.active_sources.remove(&m2.id);
        let snapshot_after_unload = engine.build_snapshot(false);
        let shortcuts_after_unload: Vec<_> = snapshot_after_unload
            .descriptors
            .iter()
            .filter(|d| d.surface == ContributionSurface::Shortcut)
            .collect();
        assert_eq!(shortcuts_after_unload.len(), 1);
        assert_eq!(
            shortcuts_after_unload[0].id.to_string(),
            "io.github.alice.weather/toggle-shortcut"
        );
    }

    #[test]
    fn test_worker_descriptor_projection_search_providers() {
        let manifest_toml = r#"
            id = "io.github.search.web"
            name = "Web Search Engine"
            version = "1.0.0"
            schema_version = 1

            [[contributions.actions]]
            id = "web-provider"
            name = "Web Search Provider"
        "#;
        let manifest = ExtensionManifest::from_toml(manifest_toml).unwrap();
        let temp_dir = std::env::temp_dir();
        let clock = Arc::new(FakeMonotonicClock::new(Instant::now()));
        let mut engine = ExtensionEngine::new_with_clock(
            InMemoryRuntime::new(),
            CatalogPaths::new(&temp_dir, &temp_dir),
            clock.clone(),
        )
        .unwrap();

        engine.active_sources.insert(
            manifest.id.clone(),
            ActiveSource {
                manifest: manifest.clone(),
                root: PathBuf::from("/tmp/web-search"),
                grants: Vec::new(),
                fingerprint: 42,
            },
        );

        let snapshot = engine.build_snapshot(false);
        let action_descriptors: Vec<_> = snapshot
            .descriptors
            .iter()
            .filter(|d| d.surface == ContributionSurface::Action)
            .collect();

        assert_eq!(action_descriptors.len(), 1);
        let desc = action_descriptors[0];
        assert_eq!(desc.id.to_string(), "io.github.search.web/web-provider");
        assert_eq!(desc.id.extension_id.as_str(), "io.github.search.web");
        assert_eq!(desc.id.contribution_id.as_str(), "web-provider");
        assert_eq!(desc.extension_name, "Web Search Engine");
        assert_eq!(desc.name, "Web Search Provider");
        assert_eq!(desc.surface, ContributionSurface::Action);
        assert_eq!(desc.runtime_kind, ExtensionRuntimeKind::Wasm);
    }

    #[test]
    fn test_worker_tick_advances_circuit_and_projects_wasm_extensions() {
        let manifest_toml = r#"
            id = "io.github.test.tick"
            name = "Tick Test"
            version = "1.0.0"
        "#;
        let manifest = ExtensionManifest::from_toml(manifest_toml).unwrap();
        let temp_dir = std::env::temp_dir();
        let clock = Arc::new(FakeMonotonicClock::new(Instant::now()));
        let mut engine = ExtensionEngine::new_with_clock(
            InMemoryRuntime::new(),
            CatalogPaths::new(&temp_dir, &temp_dir),
            clock.clone(),
        )
        .unwrap();

        engine.active_sources.insert(
            manifest.id.clone(),
            ActiveSource {
                manifest: manifest.clone(),
                root: PathBuf::from("/tmp/tick-test"),
                grants: Vec::new(),
                fingerprint: 1,
            },
        );

        // Initially Closed in snapshot
        let snapshot = engine.build_snapshot(false);
        assert_eq!(snapshot.wasm_extensions.len(), 1);
        assert_eq!(snapshot.wasm_extensions[0].state, CircuitStateKind::Closed);

        // Trip the circuit
        engine.session.host.circuit_breaker_mut().record_failure(
            &manifest.id,
            DiagnosticCode::RuntimeTrap,
            "f1",
        );
        engine.session.host.circuit_breaker_mut().record_failure(
            &manifest.id,
            DiagnosticCode::RuntimeTrap,
            "f2",
        );
        engine.session.host.circuit_breaker_mut().record_failure(
            &manifest.id,
            DiagnosticCode::RuntimeTrap,
            "f3",
        );

        // Trip generates an update on tick with open notice and Open state in snapshot
        let trip_update = engine
            .tick()
            .expect("trip generates update with open notice");
        assert_eq!(trip_update.circuit_notices.len(), 1);
        assert_eq!(
            trip_update.snapshot.unwrap().wasm_extensions[0].state,
            CircuitStateKind::Open
        );

        // Next tick when no transition is due produces None
        assert!(engine.tick().is_none());

        // Advance time by 30s through the same injected clock used by admission.
        clock.advance(Duration::from_secs(35));

        // Now tick returns an update with the new snapshot transitioning to HalfOpen
        let update = engine
            .tick()
            .expect("tick should return update on visible change");
        assert!(update.snapshot.is_some());
        let snap = update.snapshot.unwrap();
        assert_eq!(snap.wasm_extensions[0].state, CircuitStateKind::HalfOpen);
    }

    #[test]
    fn test_handle_dev_reload_success_and_atomic_swap() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let manifest_content = r#"
            schema_version = 1
            id = "org.shilpo.dev-test"
            name = "Dev Test"
            version = "0.1.0"
            api_version = "0.1.0"
            min_shilpo_version = "0.1.0"

            [[contributions.bar_widgets]]
            id = "widget1"
            name = "Widget 1"
        "#;
        fs::write(root.join("extension.toml"), manifest_content).unwrap();
        fs::write(root.join("extension.wasm"), b"WASM_DUMMY_BYTECODE").unwrap();

        let clock = Arc::new(FakeMonotonicClock::new(Instant::now()));
        let mut engine = ExtensionEngine::new_with_clock(
            InMemoryRuntime::new(),
            CatalogPaths::new(&root, &root),
            clock,
        )
        .unwrap();

        let initial_gen = engine.generation();
        let ext_id = ExtensionId::new("org.shilpo.dev-test").unwrap();

        let outcome = engine.handle_dev_reload(
            "session-1".into(),
            ext_id.clone(),
            root.clone(),
            root.join("extension.wasm"),
            1,
        );

        assert_eq!(outcome.outcome, "applied");
        assert_eq!(outcome.diagnostic_code, "OK");
        assert!(outcome.engine_generation > initial_gen);
        assert!(engine.active_dev_overrides.contains_key(&ext_id));
        assert!(engine.session.manifests.contains_key(&ext_id));

        let snapshot = engine.build_snapshot(false);
        assert!(snapshot.dev_overrides.contains(&ext_id));

        // Test unload
        let unload_update = engine.handle_dev_unload("session-1", &ext_id);
        assert!(unload_update.is_some());
        assert!(!engine.active_dev_overrides.contains_key(&ext_id));
        assert!(!engine.session.manifests.contains_key(&ext_id));
    }

    #[test]
    fn test_handle_dev_reload_fencing_stale_build_sequence() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        fs::write(
            root.join("extension.toml"),
            r#"
            schema_version = 1
            id = "org.shilpo.fence-test"
            name = "Fence Test"
            version = "0.1.0"
            api_version = "0.1.0"
            min_shilpo_version = "0.1.0"
            "#,
        )
        .unwrap();
        fs::write(root.join("extension.wasm"), b"WASM_DUMMY_BYTECODE").unwrap();

        let clock = Arc::new(FakeMonotonicClock::new(Instant::now()));
        let mut engine = ExtensionEngine::new_with_clock(
            InMemoryRuntime::new(),
            CatalogPaths::new(&root, &root),
            clock,
        )
        .unwrap();

        let ext_id = ExtensionId::new("org.shilpo.fence-test").unwrap();

        // First build sequence 5
        let res1 = engine.handle_dev_reload(
            "session-fenced".into(),
            ext_id.clone(),
            root.clone(),
            root.join("extension.wasm"),
            5,
        );
        assert_eq!(res1.outcome, "applied");

        // Stale build sequence 5 (duplicate)
        let res2 = engine.handle_dev_reload(
            "session-fenced".into(),
            ext_id.clone(),
            root.clone(),
            root.join("extension.wasm"),
            5,
        );
        assert_eq!(res2.outcome, "rejected");
        assert_eq!(res2.diagnostic_code, "STALE_BUILD_SEQUENCE");

        // Stale build sequence 4 (older)
        let res3 = engine.handle_dev_reload(
            "session-fenced".into(),
            ext_id,
            root.clone(),
            root.join("extension.wasm"),
            4,
        );
        assert_eq!(res3.outcome, "rejected");
        assert_eq!(res3.diagnostic_code, "STALE_BUILD_SEQUENCE");
    }

    #[test]
    fn test_handle_dev_reload_security_validations() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        fs::write(
            root.join("extension.toml"),
            r#"
            schema_version = 1
            id = "org.shilpo.sec-test"
            name = "Sec Test"
            version = "0.1.0"
            api_version = "0.1.0"
            min_shilpo_version = "0.1.0"
            "#,
        )
        .unwrap();
        fs::write(root.join("extension.wasm"), b"WASM_DUMMY_BYTECODE").unwrap();

        let clock = Arc::new(FakeMonotonicClock::new(Instant::now()));
        let mut engine = ExtensionEngine::new_with_clock(
            InMemoryRuntime::new(),
            CatalogPaths::new(&root, &root),
            clock,
        )
        .unwrap();

        let ext_id = ExtensionId::new("org.shilpo.sec-test").unwrap();

        // 1. Wrong extension ID
        let wrong_id = ExtensionId::new("org.shilpo.wrong-id").unwrap();
        let res = engine.handle_dev_reload(
            "s1".into(),
            wrong_id,
            root.clone(),
            root.join("extension.wasm"),
            1,
        );
        assert_eq!(res.outcome, "rejected");
        assert_eq!(res.diagnostic_code, "ID_MISMATCH");

        // 2. Nonexistent artifact
        let res = engine.handle_dev_reload(
            "s2".into(),
            ext_id.clone(),
            root.clone(),
            root.join("nonexistent.wasm"),
            1,
        );
        assert_eq!(res.outcome, "rejected");
        assert_eq!(res.diagnostic_code, "ARTIFACT_NOT_FOUND");

        // 3. Artifact outside root
        let outside = tempfile::NamedTempFile::new().unwrap();
        let res = engine.handle_dev_reload(
            "s3".into(),
            ext_id,
            root.clone(),
            outside.path().to_path_buf(),
            1,
        );
        assert_eq!(res.outcome, "rejected");
        assert_eq!(res.diagnostic_code, "ARTIFACT_OUTSIDE_ROOT");
    }
}
