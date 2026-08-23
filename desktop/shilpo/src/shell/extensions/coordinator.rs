use std::{
    path::{Component, Path, PathBuf},
    sync::{Arc, mpsc},
    time::Duration,
};

use shilpo_ext_api::{CanonicalId, ViewTree};
use shilpo_ext_runtime::{
    CatalogPaths, ContributionDescriptor, ContributionSurface, ExtensionCommand,
    ExtensionGeneration, ExtensionSnapshot, ExtensionUpdate, HostGeneration,
    default_extension_state_dir,
};

pub struct ExtensionCoordinator {
    supervisor: Arc<super::supervisor::ExtensionSupervisor>,
    _watcher: Option<super::watcher::ExtensionWatcher>,
    _fallback_scan: Option<gpui::Task<()>>,
}

impl ExtensionCoordinator {
    pub fn init(executor: gpui::BackgroundExecutor) -> Option<Self> {
        let paths = CatalogPaths::platform_default();
        Self::init_with_paths(executor, paths)
    }

    pub fn init_with_paths(
        executor: gpui::BackgroundExecutor,
        paths: CatalogPaths,
    ) -> Option<Self> {
        let supervisor = Arc::new(super::supervisor::ExtensionSupervisor::new());
        let (command_tx, command_rx) = std::sync::mpsc::sync_channel(64);
        let supervisor_for_forward = supervisor.clone();
        executor
            .spawn(async move {
                while let Ok(cmd) = command_rx.recv() {
                    let _ = supervisor_for_forward.send_command(cmd);
                }
            })
            .detach();

        let watch_paths = vec![
            default_extension_state_dir().join("dev"),
            paths.data_dir.join("installed"),
            paths.data_dir.join("activated"),
            paths.config_dir.clone(),
        ];
        let mut watcher = None;
        let mut fallback_scan = None;
        match super::watcher::ExtensionWatcher::new(command_tx.clone(), watch_paths) {
            Ok(w) => watcher = Some(w),
            Err(error) => {
                tracing::warn!(%error, "ExtensionWatcher failed, falling back to 30s background scan");
                fallback_scan = spawn_fallback_scan(&executor, command_tx, Duration::from_secs(30));
            }
        }

        Some(Self {
            supervisor,
            _watcher: watcher,
            _fallback_scan: fallback_scan,
        })
    }

    pub fn new_with_supervisor(supervisor: super::supervisor::ExtensionSupervisor) -> Self {
        Self {
            supervisor: Arc::new(supervisor),
            _watcher: None,
            _fallback_scan: None,
        }
    }

    pub fn supervisor(&self) -> &super::supervisor::ExtensionSupervisor {
        &self.supervisor
    }

    pub fn snapshot(&self) -> ExtensionSnapshot {
        self.supervisor.snapshot()
    }

    pub fn generation(&self) -> ExtensionGeneration {
        self.supervisor.generation()
    }

    pub fn host_generation(&self) -> HostGeneration {
        self.supervisor.host_generation()
    }

    pub fn descriptors(&self) -> Vec<ContributionDescriptor> {
        self.snapshot().descriptors.to_vec()
    }

    pub fn descriptors_for(&self, surface: ContributionSurface) -> Vec<ContributionDescriptor> {
        self.descriptors()
            .into_iter()
            .filter(|descriptor| descriptor.surface == surface)
            .collect()
    }

    pub fn diagnostics(&self) -> Arc<[String]> {
        self.snapshot().diagnostics.clone()
    }

    pub fn host_diagnostics(&self) -> super::supervisor::ExtensionHostDiagnostics {
        self.supervisor.diagnostics()
    }

    pub fn view(&self, id: &CanonicalId) -> Option<ViewTree> {
        self.snapshot().views.get(id).cloned()
    }

    pub fn search(
        &self,
        canonical: &CanonicalId,
        request: &shilpo_ext_api::bindings::shilpo::extension::types::SearchRequest,
        budget: shilpo_ext_runtime::RuntimeBudget,
    ) -> Result<
        Vec<shilpo_ext_api::bindings::shilpo::extension::types::SearchCandidate>,
        super::supervisor::SearchDispatchError,
    > {
        self.supervisor.search(canonical, request, budget)
    }

    pub fn settings_schema(&self, id: &CanonicalId) -> Result<Option<serde_json::Value>, String> {
        Ok(self.snapshot().settings_schemas.get(id).cloned())
    }

    pub fn asset_path(&self, id: &CanonicalId, relative: &str) -> Result<PathBuf, String> {
        let snapshot = self.snapshot();
        let root = snapshot
            .prevalidated_asset_roots
            .get(&id.extension_id)
            .ok_or_else(|| format!("extension '{}' has no active asset root", id.extension_id))?;
        let path = safe_child(&root.join("assets"), relative)?;
        if path.is_file() {
            Ok(path)
        } else {
            Err(format!("extension asset {} is unavailable", path.display()))
        }
    }

    pub fn send_command(&self, command: ExtensionCommand) -> Result<(), String> {
        self.supervisor.send_command(command)
    }

    pub fn reload_dev(
        &self,
        session_id: String,
        extension_id: shilpo_ext_api::ExtensionId,
        canonical_root: PathBuf,
        artifact_path: PathBuf,
        build_sequence: u64,
        timeout: Duration,
    ) -> Result<shilpo_ext_runtime::DevReloadOutcome, String> {
        self.supervisor.reload_dev(
            session_id,
            extension_id,
            canonical_root,
            artifact_path,
            build_sequence,
            timeout,
        )
    }

    pub fn unload_dev(
        &self,
        session_id: String,
        extension_id: shilpo_ext_api::ExtensionId,
    ) -> Result<(), String> {
        self.supervisor.unload_dev(session_id, extension_id)
    }

    pub fn drain_updates(&self) -> Vec<ExtensionUpdate> {
        self.supervisor.drain_updates()
    }

    pub fn shutdown(
        &self,
        executor: gpui::BackgroundExecutor,
        timeout: Duration,
    ) -> gpui::Task<bool> {
        let supervisor = self.supervisor.clone();
        executor.spawn(async move { supervisor.shutdown(timeout) })
    }
}

pub fn safe_child(base: &Path, relative: &str) -> Result<PathBuf, String> {
    let rel_path = Path::new(relative);
    if rel_path.is_absolute() {
        return Err("path must be relative".into());
    }
    for component in rel_path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir => {
                return Err("path contains parent or root directory traversal".into());
            }
            Component::Prefix(_) => return Err("path contains invalid component".into()),
        }
    }
    Ok(base.join(rel_path))
}

fn spawn_fallback_scan(
    executor: &gpui::BackgroundExecutor,
    command_tx: mpsc::SyncSender<ExtensionCommand>,
    interval: Duration,
) -> Option<gpui::Task<()>> {
    let fallback_tx = command_tx.clone();
    let executor_inner = executor.clone();
    Some(executor.clone().spawn(async move {
        loop {
            executor_inner.timer(interval).await;
            if fallback_tx.send(ExtensionCommand::SourcesChanged).is_err() {
                break;
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extension_generation_increment() {
        let gen0 = ExtensionGeneration::default();
        assert_eq!(gen0.0, 0);
        let gen1 = gen0.next();
        assert_eq!(gen1.0, 1);
        assert!(gen1 > gen0);
    }

    #[test]
    fn test_safe_child_traversal_prevention() {
        let base = Path::new("/tmp/test_ext");
        assert!(safe_child(base, "../etc/passwd").is_err());
        assert!(safe_child(base, "/etc/passwd").is_err());
        let valid = safe_child(base, "assets/logo.png").unwrap();
        assert_eq!(valid, PathBuf::from("/tmp/test_ext/assets/logo.png"));
    }

    #[test]
    fn test_coordinator_snapshot_reads() {
        let supervisor = crate::shell::extensions::ExtensionSupervisor::new();
        let coordinator = ExtensionCoordinator::new_with_supervisor(supervisor);

        assert_eq!(coordinator.generation(), ExtensionGeneration(0));
        assert_eq!(coordinator.descriptors().len(), 0);
        assert_eq!(
            coordinator.descriptors_for(ContributionSurface::Bar).len(),
            0
        );
        assert_eq!(
            coordinator
                .descriptors_for(ContributionSurface::Action)
                .len(),
            0
        );
    }

    #[test]
    fn test_coordinator_init_with_paths_creates_runtime() {
        let temp_base =
            std::env::temp_dir().join(format!("shilpo_ext_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(temp_base.join("data")).unwrap();
        std::fs::create_dir_all(temp_base.join("config")).unwrap();
        let paths = CatalogPaths::new(temp_base.join("data"), temp_base.join("config"));

        let executor = gpui::TestAppContext::single().executor().clone();
        let coordinator = ExtensionCoordinator::init_with_paths(executor, paths);
        assert!(coordinator.is_some());

        drop(coordinator);
        let _ = std::fs::remove_dir_all(temp_base);
    }

    #[test]
    fn test_fallback_scan_emits_sources_changed() {
        let cx = gpui::TestAppContext::single();
        let executor = cx.executor().clone();
        let (command_tx, command_rx) = mpsc::sync_channel(16);
        let task = spawn_fallback_scan(&executor, command_tx, Duration::from_millis(10));
        assert!(task.is_some());

        // The fallback scan fires SourcesChanged on its interval.
        executor.advance_clock(Duration::from_millis(10));
        executor.tick();
        executor.tick();
        assert!(matches!(
            command_rx.try_recv(),
            Ok(ExtensionCommand::SourcesChanged)
        ));

        // Disconnecting the receiver terminates the scan loop.
        drop(command_rx);
        executor.advance_clock(Duration::from_millis(10));
        executor.tick();
        executor.tick();
        drop(task);
    }
}
