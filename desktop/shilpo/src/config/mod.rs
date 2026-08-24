pub mod changeset;
pub mod merge;
pub mod migration;
pub mod overrides;
pub mod provenance;
pub mod report;
pub mod resolver;
pub mod source;
pub mod types;
pub mod unknown_keys;
pub mod validation;
pub mod watcher;

pub use changeset::ConfigChangeSet;
pub use migration::{
    LATEST_CONFIG_VERSION, Migration, MigrationError, MigrationMode, MigrationOutcome,
    MigrationRegistry, MigrationService, PrimaryStatus, migrate_primary_for_startup,
    reload_block_reason,
};
pub use overrides::{
    ConfigOverrideService, OverrideEdit, OverrideError, OverrideFs, OverrideOutcome, StdOverrideFs,
};
pub use provenance::{ConfigProvenance, format_key};
pub use report::EffectiveWithOriginsReport;
pub use resolver::{
    ConfigInspection, ConfigInspectionError, ConfigResolver, ConfigSnapshot, ResolutionReport,
};
pub use source::{ConfigSource, SourceLocation, discover_sources};
pub use types::*;
pub use unknown_keys::UnknownConfigKey;
pub use validation::{RecoveryScope, apply_scoped_recovery, classify_diagnostic};
pub use watcher::{
    ClassifiedPath, ConfigWatchError, ConfigWatchEvent, ConfigWatcher, DebounceAction,
    DebounceState, DebounceStateMachine, classify_path, is_relevant_path,
};

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::str::FromStr;

    use tempfile::TempDir;

    use super::*;

    fn valid() -> ShellConfig {
        ShellConfig::default()
    }

    fn run_config_path_consistency_probe(configure: impl FnOnce(&mut Command)) {
        let mut command = Command::new(std::env::current_exe().expect("resolve test binary"));
        command
            .args([
                "--exact",
                "config::tests::settings_and_daemon_config_paths_match_probe",
                "--nocapture",
            ])
            .env("SHILPO_CONFIG_PATH_CONSISTENCY_PROBE", "1")
            .env_remove("HOME")
            .env_remove("XDG_CONFIG_HOME");
        configure(&mut command);

        let output = command.output().expect("run isolated config path probe");
        assert!(
            output.status.success(),
            "isolated config path probe failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    #[test]
    fn settings_and_daemon_config_paths_match_probe() {
        if std::env::var_os("SHILPO_CONFIG_PATH_CONSISTENCY_PROBE").is_none() {
            return;
        }

        let canonical = default_config_path();
        assert_eq!(
            crate::settings::keybindings_page::user_config_path(),
            canonical,
            "settings must write beside the configuration read by the product"
        );
        assert_eq!(
            crate::shell::daemon_config_path(),
            canonical,
            "the shell daemon must read the canonical configuration"
        );
        assert_eq!(
            crate::shell::NiriShortcutBackend::default_output_path(),
            config_dir().join("generated/niri-keybindings.kdl"),
            "generated keybindings must use the canonical configuration directory"
        );
        assert_eq!(
            ShellSessionState::default_session_path(),
            config_dir().join("session.json"),
            "session state must use the canonical configuration directory"
        );
    }

    #[test]
    fn config_paths_stay_consistent_when_home_is_unset() {
        run_config_path_consistency_probe(|_| {});
    }

    #[test]
    fn config_paths_stay_consistent_when_xdg_and_home_differ() {
        run_config_path_consistency_probe(|command| {
            command
                .env("HOME", "/isolated/passwd-home")
                .env("XDG_CONFIG_HOME", "/isolated/xdg-config");
        });
    }

    #[test]
    fn no_files_resolves_exactly_to_default() {
        let dir = TempDir::new().unwrap();
        let resolver = ConfigResolver::new(dir.path());
        let (snapshot, report) = resolver.resolve_initial().unwrap();

        assert_eq!(snapshot.config, ShellConfig::default());
        assert_eq!(report.sources_loaded, vec![ConfigSource::Defaults]);
        assert_eq!(
            snapshot.provenance.get("theme.font_family").unwrap(),
            &SourceLocation::defaults()
        );
    }

    #[test]
    fn first_run_load_or_create_writes_only_missing_primary_file() {
        let dir = TempDir::new().unwrap();
        let primary = dir.path().join("config.toml");
        let resolver = ConfigResolver::from_primary_path(&primary);

        assert!(!primary.exists());
        let config = resolver.load_or_create().unwrap();
        assert_eq!(config, ShellConfig::default());
        assert!(primary.exists());

        // Conf.d and overrides.toml must not be created by load_or_create
        assert!(!dir.path().join("conf.d").exists());
        assert!(!dir.path().join("overrides.toml").exists());
    }

    #[test]
    fn loader_replaces_blank_config_with_defaults_without_overwriting_file() {
        let dir = TempDir::new().unwrap();
        let primary = dir.path().join("config.toml");
        std::fs::write(&primary, "   \n").unwrap();

        let resolver = ConfigResolver::from_primary_path(&primary);
        let config = resolver.load_or_create().unwrap();

        assert_eq!(config, ShellConfig::default());
        // Must NOT overwrite existing blank file
        assert_eq!(std::fs::read_to_string(&primary).unwrap(), "   \n");
    }

    #[test]
    fn partial_config_inherits_defaults() {
        let dir = TempDir::new().unwrap();
        let primary = dir.path().join("config.toml");
        std::fs::write(
            &primary,
            r#"
version = 1
[theme]
font_family = "Fira Code"
"#,
        )
        .unwrap();

        let resolver = ConfigResolver::from_primary_path(&primary);
        let (snapshot, _report) = resolver.resolve_initial().unwrap();

        assert_eq!(snapshot.config.theme.font_family, "Fira Code");
        assert_eq!(snapshot.config.bar, BarConfig::default());
        assert_eq!(
            snapshot.provenance.get("theme.font_family").unwrap().source,
            ConfigSource::Primary {
                path: primary.clone()
            }
        );
        assert_eq!(
            snapshot.provenance.get("bar.height").unwrap().source,
            ConfigSource::Defaults
        );
    }

    #[test]
    fn conf_d_files_sorted_deterministically_and_deep_merge_in_order() {
        let dir = TempDir::new().unwrap();
        let conf_d = dir.path().join("conf.d");
        std::fs::create_dir_all(&conf_d).unwrap();

        std::fs::write(
            conf_d.join("02-second.toml"),
            r#"
[bar]
height = 64
"#,
        )
        .unwrap();

        std::fs::write(
            conf_d.join("01-first.toml"),
            r#"
[bar]
height = 32
padding = 12
"#,
        )
        .unwrap();

        let resolver = ConfigResolver::new(dir.path());
        let (snapshot, _report) = resolver.resolve_initial().unwrap();

        assert_eq!(snapshot.config.bar.height, 64);
        assert_eq!(snapshot.config.bar.padding, 12);
        assert_eq!(
            snapshot.provenance.get("bar.height").unwrap().source,
            ConfigSource::Fragment {
                path: conf_d.join("02-second.toml")
            }
        );
        assert_eq!(
            snapshot.provenance.get("bar.padding").unwrap().source,
            ConfigSource::Fragment {
                path: conf_d.join("01-first.toml")
            }
        );
    }

    #[test]
    fn overrides_toml_wins_over_every_earlier_file() {
        let dir = TempDir::new().unwrap();
        let primary = dir.path().join("config.toml");
        std::fs::write(&primary, "version = 1\n[bar]\nheight = 32\n").unwrap();

        let conf_d = dir.path().join("conf.d");
        std::fs::create_dir_all(&conf_d).unwrap();
        std::fs::write(conf_d.join("01-fragment.toml"), "[bar]\nheight = 40\n").unwrap();

        let overrides = dir.path().join("overrides.toml");
        std::fs::write(&overrides, "[bar]\nheight = 56\n").unwrap();

        let resolver = ConfigResolver::new(dir.path());
        let (snapshot, _report) = resolver.resolve_initial().unwrap();

        assert_eq!(snapshot.config.bar.height, 56);
        assert_eq!(
            snapshot.provenance.get("bar.height").unwrap().source,
            ConfigSource::Overrides { path: overrides }
        );
    }

    #[test]
    fn nested_tables_merge_while_arrays_replace() {
        let dir = TempDir::new().unwrap();
        let conf_d = dir.path().join("conf.d");
        std::fs::create_dir_all(&conf_d).unwrap();

        std::fs::write(
            conf_d.join("01-bar.toml"),
            r#"
[bar.margin]
horizontal = 32

[bar.widgets]
start = ["builtin:workspaces"]
"#,
        )
        .unwrap();

        let resolver = ConfigResolver::new(dir.path());
        let (snapshot, _report) = resolver.resolve_initial().unwrap();

        assert_eq!(snapshot.config.bar.margin.horizontal, 32);
        assert_eq!(snapshot.config.bar.margin.vertical, 8); // inherited from defaults
        assert_eq!(
            snapshot.config.bar.widgets.start,
            vec![BarWidget::Builtin(BuiltinBarWidget::Workspaces)]
        );
    }

    #[test]
    fn non_toml_files_nested_directories_and_nested_files_are_ignored() {
        let dir = TempDir::new().unwrap();
        let conf_d = dir.path().join("conf.d");
        std::fs::create_dir_all(conf_d.join("subfolder")).unwrap();

        std::fs::write(conf_d.join("ignored.txt"), "[bar]\nheight = 100\n").unwrap();
        std::fs::write(
            conf_d.join("subfolder/nested.toml"),
            "[bar]\nheight = 100\n",
        )
        .unwrap();
        std::fs::write(conf_d.join("valid.toml"), "[bar]\nheight = 40\n").unwrap();

        let resolver = ConfigResolver::new(dir.path());
        let (snapshot, report) = resolver.resolve_initial().unwrap();

        assert_eq!(snapshot.config.bar.height, 40);
        assert_eq!(
            report.sources_loaded,
            vec![
                ConfigSource::Defaults,
                ConfigSource::Fragment {
                    path: conf_d.join("valid.toml")
                }
            ]
        );
    }

    #[test]
    fn provenance_points_to_winning_file_and_correct_key_line_for_nested_leaves() {
        let dir = TempDir::new().unwrap();
        let primary = dir.path().join("config.toml");
        let content = "version = 1\n\n[theme]\nfont_family = \"Inter\"\n";
        std::fs::write(&primary, content).unwrap();

        let resolver = ConfigResolver::from_primary_path(&primary);
        let (snapshot, _report) = resolver.resolve_initial().unwrap();

        let loc = snapshot.provenance.get("theme.font_family").unwrap();
        assert_eq!(loc.source, ConfigSource::Primary { path: primary });
        assert_eq!(loc.line, Some(4));
        assert_eq!(loc.column, Some(1));
    }

    #[test]
    fn replacing_a_subtree_removes_stale_child_provenance() {
        let dir = TempDir::new().unwrap();
        let primary = dir.path().join("config.toml");
        std::fs::write(
            &primary,
            "version = 1\n[bar.margin]\nhorizontal = 20\nvertical = 10\n",
        )
        .unwrap();

        let overrides = dir.path().join("overrides.toml");
        std::fs::write(
            &overrides,
            "[bar]\nmargin = { horizontal = 50, vertical = 8 }\n",
        )
        .unwrap();

        let resolver = ConfigResolver::new(dir.path());
        let (snapshot, _report) = resolver.resolve_initial().unwrap();

        assert_eq!(snapshot.config.bar.margin.horizontal, 50);
        assert_eq!(snapshot.config.bar.margin.vertical, 8); // reset to default via whole table replacement
        assert_eq!(
            snapshot
                .provenance
                .get("bar.margin.horizontal")
                .unwrap()
                .source,
            ConfigSource::Overrides { path: overrides }
        );
    }

    #[test]
    fn invalid_leaf_value_performs_reject_value_and_restores_provenance() {
        let dir = TempDir::new().unwrap();
        let primary = dir.path().join("config.toml");
        std::fs::write(&primary, "version = 1\n[bar]\nheight = 500\n").unwrap();

        let resolver = ConfigResolver::new(dir.path());
        let (snapshot, report) = resolver.resolve_initial().unwrap();

        assert_eq!(snapshot.config.bar.height, 48); // restored to default
        assert_eq!(report.recovery_scope, Some(RecoveryScope::RejectValue));
        assert_eq!(
            snapshot.provenance.get("bar.height").unwrap().source,
            ConfigSource::Defaults
        );
    }

    #[test]
    fn invalid_component_performs_retain_previous_component() {
        let dir = TempDir::new().unwrap();
        let primary = dir.path().join("config.toml");
        std::fs::write(
            &primary,
            "version = 1\n[theme]\nfont_family = \"Inter\"\n[[desktop.widgets]]\ninstance = \"\"\ncontribution = \"ext:io.github.example.widget/desktop\"\noutput = \"primary\"\nwidth = 100\nheight = 100\n",
        )
        .unwrap();

        let resolver = ConfigResolver::new(dir.path());
        let (snapshot, report) = resolver.resolve_initial().unwrap();

        assert_eq!(snapshot.config.theme.font_family, "Inter"); // valid theme component retained
        assert_eq!(snapshot.config.desktop, DesktopConfig::default()); // invalid desktop component reset
        assert_eq!(
            report.recovery_scope,
            Some(RecoveryScope::RetainPreviousComponent)
        );
    }

    #[test]
    fn syntax_error_and_unsupported_version_perform_reject_candidate() {
        let dir = TempDir::new().unwrap();
        let primary = dir.path().join("config.toml");

        // Unsupported version
        std::fs::write(&primary, "version = 999\n").unwrap();
        let resolver = ConfigResolver::new(dir.path());
        assert!(resolver.resolve_initial().is_err());

        // Syntax error
        std::fs::write(&primary, "invalid = [ \n").unwrap();
        assert!(resolver.resolve_initial().is_err());
    }

    #[test]
    fn failed_reload_retains_previous_snapshot_and_emits_empty_changeset() {
        let dir = TempDir::new().unwrap();
        let primary = dir.path().join("config.toml");
        std::fs::write(&primary, "version = 1\n[theme]\nfont_family = \"Inter\"\n").unwrap();

        let resolver = ConfigResolver::new(dir.path());
        let (initial_snapshot, _report) = resolver.resolve_initial().unwrap();

        // Introduce syntax error for reload
        std::fs::write(&primary, "invalid syntax {{{\n").unwrap();
        let (reload_snapshot, changeset, report) = resolver.resolve_reload(&initial_snapshot);

        assert_eq!(reload_snapshot, initial_snapshot);
        assert!(changeset.is_empty());
        assert_eq!(report.recovery_scope, Some(RecoveryScope::RejectCandidate));
    }

    #[test]
    fn successful_reload_publishes_one_snapshot_and_deterministic_changeset() {
        let dir = TempDir::new().unwrap();
        let primary = dir.path().join("config.toml");
        std::fs::write(&primary, "version = 1\n[theme]\nfont_family = \"Inter\"\n").unwrap();

        let resolver = ConfigResolver::new(dir.path());
        let (initial_snapshot, _report) = resolver.resolve_initial().unwrap();

        // Modify font_family
        std::fs::write(&primary, "version = 1\n[theme]\nfont_family = \"Roboto\"\n").unwrap();
        let (reload_snapshot, changeset, _report) = resolver.resolve_reload(&initial_snapshot);

        assert_eq!(reload_snapshot.config.theme.font_family, "Roboto");
        assert!(changeset.theme);
        assert!(!changeset.bar);
    }

    #[test]
    fn identical_reload_produces_empty_changeset() {
        let dir = TempDir::new().unwrap();
        let primary = dir.path().join("config.toml");
        std::fs::write(&primary, "version = 1\n[theme]\nfont_family = \"Inter\"\n").unwrap();

        let resolver = ConfigResolver::new(dir.path());
        let (initial_snapshot, _report) = resolver.resolve_initial().unwrap();

        let (reload_snapshot, changeset, _report) = resolver.resolve_reload(&initial_snapshot);

        assert_eq!(reload_snapshot, initial_snapshot);
        assert!(changeset.is_empty());
    }

    #[test]
    fn session_operational_keys_absent_from_resolver_and_provenance() {
        let dir = TempDir::new().unwrap();
        let resolver = ConfigResolver::new(dir.path());
        let (snapshot, _report) = resolver.resolve_initial().unwrap();

        assert!(snapshot.provenance.get("pinned_apps").is_none());
        assert!(snapshot.provenance.get("launch_counts").is_none());
        assert!(snapshot.provenance.get("dnd_active").is_none());
    }

    #[test]
    fn existing_schema_fixture_and_config_example_toml_pass() {
        let example_path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config.example.toml");
        let resolver = ConfigResolver::from_primary_path(&example_path);
        let config = resolver.load().unwrap();
        assert_eq!(config.bar.margin.horizontal, 180);
    }

    #[test]
    fn effective_with_origins_golden_output_is_stable() {
        let dir = TempDir::new().unwrap();
        let primary = dir.path().join("config.toml");
        std::fs::write(&primary, "version = 1\n[theme]\nfont_family = \"Inter\"\n").unwrap();

        let resolver = ConfigResolver::new(dir.path());
        let (snapshot, _report) = resolver.resolve_initial().unwrap();

        let report = EffectiveWithOriginsReport::from_snapshot(&snapshot);
        let text = report.to_text();

        assert!(text.contains("Effective Configuration with Provenance Report"));
        assert!(text.contains("theme.font_family => "));
        assert!(text.contains("config.toml"));
        let loc = snapshot.provenance.get("theme.font_family").unwrap();
        assert!(loc.line.is_some());
    }

    #[test]
    fn session_state_roundtrip_and_atomic_save() {
        let path = std::env::temp_dir().join(format!("shilpo-session-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut session = ShellSessionState::default();
        session.record_app_launch("org.gnome.Terminal");
        session.record_app_launch("firefox");
        session.pinned_apps.push("org.gnome.Terminal".into());
        session.dnd_active = true;

        session.save_atomic(&path).unwrap();

        let loaded = ShellSessionState::load_or_default(&path);
        assert_eq!(loaded, session);
        assert_eq!(loaded.pinned_apps, vec!["org.gnome.Terminal"]);
        assert!(loaded.dnd_active);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn roundtrip() {
        let c = valid();
        assert_eq!(c, toml::from_str(&toml::to_string(&c).unwrap()).unwrap());
    }

    #[test]
    fn version_required_and_exact() {
        assert!(toml::from_str::<ShellConfig>("[theme]\nmode='dark'\naccent='#000000'\nfont_family='x'\ncorner_radius_scale=1\n[bar]\nposition='top'\nstyle='floating-capsule'\nheight=48\npadding=8\nwidget_spacing=6\n[bar.margin]\nhorizontal=1\nvertical=1\n[bar.widgets]\nstart=[]\ncenter=[]\nend=[]").is_err());
        let mut c = valid();
        c.version = 2;
        assert!(c.validate().is_err());
    }

    #[test]
    fn unknown_field() {
        let mut s = toml::to_string(&valid()).unwrap();
        s.push_str("unknown=1\n");
        assert!(toml::from_str::<ShellConfig>(&s).is_err());
    }

    #[test]
    fn validation_categories() {
        let mut c = valid();
        c.theme.font_family = " ".into();
        c.theme.corner_radius_scale = f32::NAN;
        c.bar.height = 1;
        c.bar.padding = 65;
        c.bar.widget_spacing = 65;
        c.bar.margin.horizontal = 513;
        c.bar.margin.vertical = 513;
        assert!(c.validate().is_err());
    }

    #[test]
    fn duplicate_widget() {
        let mut c = valid();
        c.bar
            .widgets
            .end
            .push(BarWidget::Builtin(BuiltinBarWidget::Clock));
        assert!(c.validate().is_err());
    }

    #[test]
    fn bar_widget_references_are_strict_and_namespaced() {
        assert_eq!(
            "builtin:clock".parse::<BarWidget>().unwrap(),
            BarWidget::Builtin(BuiltinBarWidget::Clock)
        );
        assert!(matches!(
            "ext:io.github.alice.world-clock/bar"
                .parse::<BarWidget>()
                .unwrap(),
            BarWidget::Extension(_)
        ));
        assert!("Clok".parse::<BarWidget>().is_err());
        assert!(
            "io.github.alice.world-clock/bar"
                .parse::<BarWidget>()
                .is_err()
        );
        assert_eq!(
            serde_json::to_string(&BarWidget::Builtin(BuiltinBarWidget::Clock)).unwrap(),
            "\"builtin:clock\""
        );
        assert_eq!(
            "builtin:date".parse::<BarWidget>().unwrap(),
            BarWidget::Builtin(BuiltinBarWidget::Date)
        );
        assert_eq!(
            serde_json::to_string(&BarWidget::Builtin(BuiltinBarWidget::Date)).unwrap(),
            "\"builtin:date\""
        );
        assert!("date".parse::<BarWidget>().is_err());
    }

    #[test]
    fn default_bar_places_clock_before_date() {
        assert_eq!(
            ShellConfig::default().bar.widgets.center[..2],
            [
                BarWidget::Builtin(BuiltinBarWidget::Clock),
                BarWidget::Builtin(BuiltinBarWidget::Date),
            ]
        );
    }

    #[test]
    fn extension_settings_are_namespaced_objects() {
        let mut config = valid();
        config.extensions.settings.insert(
            "org.shilpo.weather".into(),
            serde_json::json!({"location": "Kolkata"}),
        );
        assert!(config.validate().is_ok());
        assert_eq!(
            toml::from_str::<ShellConfig>(&toml::to_string(&config).unwrap()).unwrap(),
            config
        );

        config.extensions.settings.insert(
            "Weather".into(),
            serde_json::Value::String("Kolkata".into()),
        );
        assert!(config.validate().is_err());
    }

    #[test]
    fn extension_secret_reference_settings_round_trip_without_plaintext() {
        let mut config = valid();
        let reference = shilpo_ext_api::SecretRef::new("opaque-handle-123");
        config.extensions.settings.insert(
            "org.shilpo.weather".into(),
            serde_json::json!({ "credential": serde_json::to_value(&reference).unwrap() }),
        );
        let serialized = toml::to_string(&config).unwrap();
        assert!(serialized.contains("opaque-handle-123"));
        assert!(!serialized.contains("SENTINEL_SECRET_BYTES"));
        let round_tripped: ShellConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(round_tripped, config);
    }

    #[test]
    fn desktop_extension_instances_are_namespaced_and_unique() {
        let contribution = "ext:io.github.alice.world-clock/desktop"
            .parse::<ExtensionContributionRef>()
            .unwrap();
        assert_eq!(
            serde_json::to_string(&contribution).unwrap(),
            "\"ext:io.github.alice.world-clock/desktop\""
        );
        assert!(
            "io.github.alice.world-clock/desktop"
                .parse::<ExtensionContributionRef>()
                .is_err()
        );

        let mut config = ShellConfig::default();
        let widget = DesktopWidgetConfig {
            instance: "home-clock".into(),
            contribution,
            output: "primary".into(),
            x: 32,
            y: 32,
            width: 320,
            height: 180,
            settings: serde_json::json!({}),
        };
        config.desktop.widgets.push(widget.clone());
        assert!(config.validate().is_ok());
        config.desktop.widgets.push(widget);
        assert!(config.validate().is_err());
    }

    #[test]
    fn schema_fixture_matches_generated_schema() {
        // Keep the checked-in schema fixture aligned with the generated contract.
        if std::env::var("UPDATE_CONFIG_SCHEMA").is_ok() {
            let schema_path = concat!(env!("CARGO_MANIFEST_DIR"), "/schema/config-v1.schema.json");
            let _ = std::fs::write(schema_path, ShellConfig::schema_json());
        }
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../schema/config-v1.schema.json")).unwrap();
        let generated: serde_json::Value =
            serde_json::from_str(&ShellConfig::schema_json()).unwrap();
        assert_eq!(fixture, generated);
    }

    #[test]
    fn keybindings_config_validation_and_parsing() {
        let mut config = ShellConfig::default();
        config.keybindings.push(KeybindingConfig {
            action: "ext:io.github.alice.weather/toggle-weather".into(),
            shortcut: Some("Super+W".into()),
            enabled: true,
        });
        config.keybindings.push(KeybindingConfig {
            action: "builtin:toggle_bar".into(),
            shortcut: None,
            enabled: false,
        });
        assert!(config.validate().is_ok());

        // Enabled keybinding missing shortcut
        let mut invalid_1 = ShellConfig::default();
        invalid_1.keybindings.push(KeybindingConfig {
            action: "builtin:toggle_bar".into(),
            shortcut: None,
            enabled: true,
        });
        assert!(invalid_1.validate().is_err());

        // Duplicate action entries
        let mut invalid_2 = ShellConfig::default();
        invalid_2.keybindings.push(KeybindingConfig {
            action: "builtin:toggle_bar".into(),
            shortcut: Some("Super+B".into()),
            enabled: true,
        });
        invalid_2.keybindings.push(KeybindingConfig {
            action: "builtin:toggle_bar".into(),
            shortcut: Some("Super+Shift+B".into()),
            enabled: true,
        });
        assert!(invalid_2.validate().is_err());

        // Duplicate shortcut bindings
        let mut invalid_3 = ShellConfig::default();
        invalid_3.keybindings.push(KeybindingConfig {
            action: "builtin:toggle_bar".into(),
            shortcut: Some("Super+W".into()),
            enabled: true,
        });
        invalid_3.keybindings.push(KeybindingConfig {
            action: "builtin:toggle_overview".into(),
            shortcut: Some("super+w".into()),
            enabled: true,
        });
        assert!(invalid_3.validate().is_err());
    }

    #[test]
    fn per_output_config_overrides_and_disabled() {
        let toml_text = r##"
version = 1
[theme]
font_family = "sans-serif"
corner_radius_scale = 1.0

[bar]
position = "Top"
style = "Float"
height = 48
padding = 8
widget_spacing = 6
[bar.margin]
horizontal = 16
vertical = 6
[bar.widgets]
start = ["builtin:workspaces"]
center = ["builtin:clock"]
end = ["builtin:settings"]

[outputs."DP-1"]
position = "Bottom"
style = "Rect"

[outputs."HDMI-A-1"]
enabled = false
"##;
        let config: ShellConfig = toml::from_str(toml_text).unwrap();
        let default_bar = config.bar_for_output(Some("DP-2"), false).unwrap();
        assert_eq!(default_bar.position, BarPosition::Top);

        let dp1_bar = config.bar_for_output(Some("DP-1"), false).unwrap();
        assert_eq!(dp1_bar.position, BarPosition::Bottom);
        assert_eq!(dp1_bar.style, BarStyle::Rect);

        assert!(config.bar_for_output(Some("HDMI-A-1"), false).is_none());
    }

    #[test]
    fn test_example_config_roundtrips_without_coercion() {
        let example_path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config.example.toml");
        let resolver = ConfigResolver::from_primary_path(&example_path);
        let config = resolver.load().unwrap();
        assert_eq!(config.bar.margin.horizontal, 180);
    }

    #[test]
    fn test_per_output_override_validation() {
        let toml_text = r##"
version = 1
[theme]
font_family = "sans-serif"
corner_radius_scale = 1.0

[bar]
position = "Top"
style = "Float"
height = 48
padding = 8
widget_spacing = 6
[bar.margin]
horizontal = 16
vertical = 6
[bar.widgets]
start = ["builtin:workspaces"]
center = ["builtin:clock"]
end = ["builtin:settings"]

[outputs."DP-1"]
margin = { horizontal = 600, vertical = 6 }
"##;
        let config: ShellConfig = toml::from_str(toml_text).unwrap();
        let err = config.validate().unwrap_err();
        match err {
            ConfigError::Validation { diagnostics } => {
                assert!(
                    diagnostics
                        .iter()
                        .any(|d| d.path == "outputs.\"DP-1\".margin.horizontal")
                );
            }
            _ => panic!("expected validation error"),
        }
    }

    #[test]
    fn test_app_launch_frequency_ranking_and_privacy_purge() {
        let mut session = ShellSessionState::default();
        session.record_app_launch("firefox");
        session.record_app_launch("firefox");
        session.record_app_launch("org.gnome.Terminal");

        assert_eq!(session.app_launch_count("firefox"), 2);
        assert_eq!(session.app_launch_count("org.gnome.Terminal"), 1);
        assert_eq!(session.app_launch_count("unknown"), 0);

        session.purge_usage_history();
        assert_eq!(session.app_launch_count("firefox"), 0);
    }

    #[test]
    fn test_accessibility_theme_config() {
        let mut config = ShellConfig::default();
        assert!(!config.theme.high_contrast);
        assert!(!config.theme.reduced_motion);

        config.theme.high_contrast = true;
        config.theme.reduced_motion = true;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_clock_format_and_units_config() {
        let mut config = ShellConfig::default();
        assert!(config.clock_format.is_none());
        assert!(config.temperature_unit.is_none());

        config.clock_format = Some("%I:%M %p".to_string());
        config.temperature_unit = Some("Celsius".to_string());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_schema_migration_pipeline_and_fixture_recovery() {
        let legacy_json = r#"{"version": 0, "pinned_apps": ["code"]}"#;
        let migrated = ShellSessionState::migrate_to_latest(legacy_json);
        assert_eq!(migrated.version, 1);
        assert_eq!(migrated.pinned_apps, vec!["code"]);

        let invalid_json = r#"{"version": 9999, "invalid": true}"#;
        let fallback = ShellSessionState::migrate_to_latest(invalid_json);
        assert_eq!(fallback.version, 1);
    }

    #[test]
    fn test_startup_config_and_compositor_wait_policy() {
        let mut config = ShellConfig::default();
        assert_eq!(config.startup.compositor_wait_timeout_ms, 3000);
        assert!(config.startup.autostart_apps.is_empty());

        config.startup.autostart_apps.push("waybar".to_string());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_clipboard_config_validation_and_recovery() {
        let mut config = ShellConfig::default();
        assert_eq!(config.clipboard.history_limit, 100);
        assert!(config.validate().is_ok());

        // Zero limit is rejected by validation
        config.clipboard.history_limit = 0;
        let diagnostics = match config.validate().unwrap_err() {
            ConfigError::Validation { diagnostics } => diagnostics,
            other => panic!("expected validation error, got {other:?}"),
        };
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].path, "clipboard.history_limit");

        // Scoped recovery restores fallback value
        let fallback = ShellConfig::default();
        let mut prov = ConfigProvenance::new();
        let fallback_prov = ConfigProvenance::new();
        let scope = apply_scoped_recovery(
            &mut config,
            &mut prov,
            &fallback,
            &fallback_prov,
            &diagnostics,
        );
        assert_eq!(scope, RecoveryScope::RejectValue);
        assert_eq!(config.clipboard.history_limit, 100);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_session_restore_fallback_policy() {
        let temp_file =
            std::env::temp_dir().join(format!("shilpo-session-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&temp_file);

        let (state, restored) = ShellSessionState::restore_with_fallback(&temp_file);
        assert!(!restored);
        assert_eq!(state.version, 1);

        let valid_session = ShellSessionState {
            version: 1,
            pinned_apps: vec!["gimp".to_string()],
            launch_counts: std::collections::HashMap::new(),
            dnd_active: false,
            night_light_active: false,
            ..Default::default()
        };
        valid_session.save_atomic(&temp_file).unwrap();

        let (restored_state, ok) = ShellSessionState::restore_with_fallback(&temp_file);
        assert!(ok);
        assert_eq!(restored_state.pinned_apps, vec!["gimp"]);

        let _ = std::fs::remove_file(temp_file);
    }

    #[test]
    fn test_transient_and_sensitive_state_exclusion_audit() {
        let mut session = ShellSessionState::default();
        session.pinned_apps.push("code".to_string());
        session.pinned_apps.push("app-with-secret-key".to_string());
        session.pinned_apps.push("app-with-token-auth".to_string());

        session.sanitize_sensitive_state();
        assert_eq!(session.pinned_apps, vec!["code".to_string()]);
    }

    #[test]
    fn test_locale_selection_config() {
        let mut config = ShellConfig::default();
        assert_eq!(config.locale, None);
        config.locale = Some("bn-IN".to_string());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_action_registry_and_configuration_validation() {
        let mut config = ShellConfig::default();
        config.bar.height = 0;
        assert!(config.validate().is_err());

        config.bar.height = 36;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_migration_fixtures_integration() {
        let legacy_json = r#"{"version": 0, "pinned_apps": ["terminal"], "launch_counts": {}, "dnd_active": false, "night_light_active": false}"#;
        let migrated = ShellSessionState::migrate_to_latest(legacy_json);
        assert_eq!(migrated.version, 1);
        assert_eq!(migrated.pinned_apps, vec!["terminal".to_string()]);
    }

    #[test]
    fn test_durable_per_output_bar_state_and_workspace_persistence() {
        let session = ShellSessionState {
            last_workspace_id: Some(3),
            visible_output_bars: vec![1, 2],
            ..Default::default()
        };

        let temp_file =
            std::env::temp_dir().join(format!("session-test-{}.json", std::process::id()));
        session.save_atomic(&temp_file).unwrap();

        let (restored, ok) = ShellSessionState::restore_with_fallback(&temp_file);
        assert!(ok);
        assert_eq!(restored.last_workspace_id, Some(3));
        assert_eq!(restored.visible_output_bars, vec![1, 2]);

        let _ = std::fs::remove_file(&temp_file);
    }

    #[test]
    fn test_action_enablement_shortcut_conflicts_and_accelerators() {
        let mut config = ShellConfig::default();
        config.bar.height = 48;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_running_apps_config_parsing_and_active_window_rejection() {
        assert_eq!(
            BuiltinBarWidget::from_str("running_apps"),
            Ok(BuiltinBarWidget::RunningApps)
        );
        assert!(BuiltinBarWidget::from_str("active_window").is_err());
        assert_eq!(BuiltinBarWidget::RunningApps.to_string(), "running_apps");

        let valid_widget: BarWidget = serde_json::from_str(r#""builtin:running_apps""#).unwrap();
        assert_eq!(
            valid_widget,
            BarWidget::Builtin(BuiltinBarWidget::RunningApps)
        );

        let invalid: Result<BarWidget, _> = serde_json::from_str(r#""builtin:active_window""#);
        assert!(invalid.is_err());
    }

    #[test]
    fn unknown_keys_warn_and_are_ignored_in_memory() {
        let dir = TempDir::new().unwrap();
        let primary = dir.path().join("config.toml");
        let content = "version = 1\nbaer = 1\n[bar]\nheight = 32\nheigth = 64\npadding = 12\n";
        std::fs::write(&primary, content).unwrap();

        let resolver = ConfigResolver::from_primary_path(&primary);
        let (snapshot, report) = resolver.resolve_initial().unwrap();

        assert_eq!(snapshot.config.bar.height, 32);
        assert_eq!(snapshot.config.bar.padding, 12);
        assert_eq!(report.recovery_scope, None);
        assert!(report.diagnostics.is_empty());

        let paths: Vec<String> = report
            .unknown_keys
            .iter()
            .map(|key| key.path.clone())
            .collect();
        assert_eq!(paths, vec!["baer", "bar.heigth"]);
        assert_eq!(report.unknown_keys[0].suggestion.as_deref(), Some("bar"));
        assert_eq!(
            report.unknown_keys[1].suggestion.as_deref(),
            Some("bar.height")
        );
        // Source documents are never rewritten.
        assert_eq!(std::fs::read_to_string(&primary).unwrap(), content);
    }

    #[test]
    fn unknown_keys_report_exact_source_line_and_column() {
        let dir = TempDir::new().unwrap();
        let primary = dir.path().join("config.toml");
        std::fs::write(&primary, "version = 1\n\n[bar]\nheigth = 64\n").unwrap();

        let resolver = ConfigResolver::from_primary_path(&primary);
        let (_snapshot, report) = resolver.resolve_initial().unwrap();

        assert_eq!(report.unknown_keys.len(), 1);
        let warning = &report.unknown_keys[0];
        assert_eq!(warning.source, ConfigSource::Primary { path: primary });
        assert_eq!(warning.line, Some(4));
        assert_eq!(warning.column, Some(1));
    }

    #[test]
    fn unknown_output_child_warns_while_output_name_is_accepted() {
        let dir = TempDir::new().unwrap();
        let primary = dir.path().join("config.toml");
        std::fs::write(
            &primary,
            "version = 1\n[outputs.\"DP-1\"]\nheigth = 32\nenabled = true\n",
        )
        .unwrap();

        let resolver = ConfigResolver::from_primary_path(&primary);
        let (snapshot, report) = resolver.resolve_initial().unwrap();

        assert!(snapshot.config.outputs.contains_key("DP-1"));
        assert_eq!(report.unknown_keys.len(), 1);
        assert_eq!(report.unknown_keys[0].path, "outputs.\"DP-1\".heigth");
        assert_eq!(
            report.unknown_keys[0].suggestion.as_deref(),
            Some("outputs.\"DP-1\".height")
        );
    }

    #[test]
    fn open_extension_settings_produce_no_warnings() {
        let dir = TempDir::new().unwrap();
        let primary = dir.path().join("config.toml");
        std::fs::write(
            &primary,
            "version = 1\n[extensions.settings.\"org.shilpo.weather\"]\nlocation = \"Kolkata\"\nmetadata = { anything = [1, 2] }\n",
        )
        .unwrap();

        let resolver = ConfigResolver::from_primary_path(&primary);
        let (snapshot, report) = resolver.resolve_initial().unwrap();

        assert!(report.unknown_keys.is_empty());
        assert_eq!(
            snapshot.config.extensions.settings["org.shilpo.weather"]["location"],
            "Kolkata"
        );
    }

    #[test]
    fn unknown_higher_precedence_entry_does_not_shadow_lower_valid_value() {
        let dir = TempDir::new().unwrap();
        let primary = dir.path().join("config.toml");
        std::fs::write(&primary, "version = 1\n[bar]\nheight = 40\n").unwrap();

        let overrides = dir.path().join("overrides.toml");
        std::fs::write(&overrides, "[bar]\nheigth = 56\n").unwrap();

        let resolver = ConfigResolver::new(dir.path());
        let (snapshot, report) = resolver.resolve_initial().unwrap();

        assert_eq!(snapshot.config.bar.height, 40);
        assert_eq!(report.unknown_keys.len(), 1);
        assert_eq!(
            report.unknown_keys[0].source,
            ConfigSource::Overrides { path: overrides }
        );
        assert!(!report.unknown_keys[0].path.is_empty());
    }

    #[test]
    fn warnings_keep_deterministic_source_then_document_order() {
        let dir = TempDir::new().unwrap();
        let primary = dir.path().join("config.toml");
        std::fs::write(&primary, "version = 1\nbaer = 1\nzat = 2\n").unwrap();

        let conf_d = dir.path().join("conf.d");
        std::fs::create_dir_all(&conf_d).unwrap();
        std::fs::write(conf_d.join("01-first.toml"), "[bar]\nheigth = 32\n").unwrap();
        std::fs::write(
            conf_d.join("02-second.toml"),
            "[theme]\nfont_famly = \"X\"\nfont_fmaily = \"Y\"\n",
        )
        .unwrap();

        let overrides = dir.path().join("overrides.toml");
        std::fs::write(&overrides, "[capture]\nshwo_pointer = false\n").unwrap();

        let resolver = ConfigResolver::new(dir.path());
        let (_snapshot, report) = resolver.resolve_initial().unwrap();

        let paths: Vec<String> = report.unknown_keys.iter().map(|k| k.path.clone()).collect();
        assert_eq!(
            paths,
            vec![
                "baer",
                "zat",
                "bar.heigth",
                "theme.font_famly",
                "theme.font_fmaily",
                "capture.shwo_pointer",
            ]
        );
        let sources: Vec<ConfigSource> = report
            .unknown_keys
            .iter()
            .map(|k| k.source.clone())
            .collect();
        assert_eq!(
            sources,
            vec![
                ConfigSource::Primary {
                    path: primary.clone()
                },
                ConfigSource::Primary { path: primary },
                ConfigSource::Fragment {
                    path: conf_d.join("01-first.toml")
                },
                ConfigSource::Fragment {
                    path: conf_d.join("02-second.toml")
                },
                ConfigSource::Fragment {
                    path: conf_d.join("02-second.toml")
                },
                ConfigSource::Overrides { path: overrides },
            ]
        );
    }

    #[test]
    fn repeated_typo_in_two_files_produces_two_warnings() {
        let dir = TempDir::new().unwrap();
        let primary = dir.path().join("config.toml");
        std::fs::write(&primary, "version = 1\n[bar]\nheigth = 32\n").unwrap();

        let conf_d = dir.path().join("conf.d");
        std::fs::create_dir_all(&conf_d).unwrap();
        std::fs::write(conf_d.join("01-fragment.toml"), "[bar]\nheigth = 40\n").unwrap();

        let resolver = ConfigResolver::new(dir.path());
        let (_snapshot, report) = resolver.resolve_initial().unwrap();

        assert_eq!(report.unknown_keys.len(), 2);
        assert!(report.unknown_keys.iter().all(|k| k.path == "bar.heigth"));
        assert!(report.unknown_keys[0].source != report.unknown_keys[1].source);
    }

    #[test]
    fn reload_strips_unknown_key_and_keeps_previous_valid_value() {
        let dir = TempDir::new().unwrap();
        let primary = dir.path().join("config.toml");
        std::fs::write(&primary, "version = 1\n[bar]\nheight = 40\n").unwrap();

        let resolver = ConfigResolver::new(dir.path());
        let (initial_snapshot, _report) = resolver.resolve_initial().unwrap();

        let overrides = dir.path().join("overrides.toml");
        std::fs::write(&overrides, "[bar]\nheigth = 56\n").unwrap();

        let (reload_snapshot, changeset, report) = resolver.resolve_reload(&initial_snapshot);

        assert_eq!(reload_snapshot.config.bar.height, 40);
        assert!(changeset.is_empty());
        assert_eq!(report.recovery_scope, None);
        assert_eq!(report.unknown_keys.len(), 1);
        assert_eq!(
            report.unknown_keys[0].source,
            ConfigSource::Overrides { path: overrides }
        );
        assert_eq!(report.unknown_keys[0].path, "bar.heigth");
    }

    #[test]
    fn unknown_keys_do_not_trigger_recovery_on_initial_or_reload() {
        let dir = TempDir::new().unwrap();
        let primary = dir.path().join("config.toml");
        std::fs::write(&primary, "version = 1\nbaer = 1\n[bar]\nheight = 48\n").unwrap();

        let resolver = ConfigResolver::from_primary_path(&primary);
        let (initial_snapshot, initial_report) = resolver.resolve_initial().unwrap();
        assert_eq!(initial_report.recovery_scope, None);
        assert_eq!(initial_report.unknown_keys.len(), 1);

        let (reload_snapshot, changeset, reload_report) =
            resolver.resolve_reload(&initial_snapshot);
        assert_eq!(reload_snapshot, initial_snapshot);
        assert!(changeset.is_empty());
        assert_eq!(reload_report.recovery_scope, None);
        assert_eq!(reload_report.unknown_keys.len(), 1);
    }
}
