pub mod effects;
pub mod events;
pub mod id;
pub mod manifest;
pub mod view;

#[allow(clippy::too_many_arguments)]
pub mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "extension",
        additional_derives: [serde::Serialize, serde::Deserialize, PartialEq],
    });
}

pub use effects::{HostOperation, WallpaperSource};
pub use events::{
    BarMenuCloseReason, EventKind, ExtensionEvent, WallpaperRequest, WallpaperRequestReason,
    WallpaperTarget, WorkspaceTarget,
};
pub use id::{CanonicalId, ContributionId, ExtensionId, IdError};
pub use manifest::{
    ActionContribution, BackgroundTaskContribution, BarMenuContribution, BarWidgetContribution,
    Capability, CapabilityKind, Contributions, DesktopWidgetContribution, ExtensionManifest,
    LibraryConfig, ManifestError, OFFICIAL_AUTHOR, SUPPORTED_API_VERSION, SUPPORTED_SCHEMA_VERSION,
    SearchProviderContribution, SearchProviderMode, SecretPurpose, SecretRef,
    SettingsPageContribution, SidePanelContribution, Subscription, WallpaperMode,
    WallpaperProviderContribution, WallpaperTargetKind, valid_virtual_path_pattern,
    validate_author, wildcard_matches,
};
pub use view::{
    Alignment, BadgeNode, ButtonNode, ContainerDirection, ContainerNode, CornerRadii, EdgeInsets,
    Fill, IconButtonNode, IconNode, ImageNode, Justification, ListNode, LoadingIndicatorNode,
    Overflow, ProgressNode, SemanticColorToken, ShadowStyle, SliderNode, SpacerNode, TextAlign,
    TextInputNode, TextNode, ToggleNode, ViewLimits, ViewNode, ViewStyle, ViewTree,
    ViewValidationError,
};

#[cfg(test)]
mod contract_tests {
    use super::*;

    const MANIFEST: &str = r#"
        id = "io.github.alice.world-clock"
        name = "World Clock"
        version = "1.0.0"
        schema_version = 1
        api_version = "0.1.0"

        [[contributions.bar_widgets]]
        id = "bar"
        name = "World Clock"

        [[subscriptions]]
        event = "timer_fired"

        [[capabilities]]
        kind = "events:subscribe"
        events = ["timer_fired"]
    "#;

    #[test]
    fn manifest_validation_rejects_invalid_ids_and_duplicate_contributions() {
        let invalid_id =
            MANIFEST.replace("io.github.alice.world-clock", "IO.github.alice.world clock");
        assert!(matches!(
            ExtensionManifest::from_toml(&invalid_id),
            Err(ManifestError::ParseError(_))
        ));

        let duplicate = MANIFEST.replace(
            "[[subscriptions]]",
            "[[contributions.actions]]\nid = \"bar\"\nname = \"Duplicate\"\n\n[[subscriptions]]",
        );
        assert!(matches!(
            ExtensionManifest::from_toml(&duplicate),
            Err(ManifestError::Validation(message)) if message.contains("duplicate contribution")
        ));
    }

    #[test]
    fn view_validation_rejects_unsafe_and_overdeep_trees() {
        let unsafe_view = ViewTree::new(ViewNode::Image(ImageNode {
            asset_path: "../secret.png".into(),
            width: None,
            height: None,
            style: None,
        }));
        assert!(unsafe_view.validate(ViewLimits::default()).is_err());

        let deep_view = (0..4).fold(ViewNode::Divider, |child, _| {
            ViewNode::Container(ContainerNode {
                direction: ContainerDirection::Column,
                children: vec![child],
                style: None,
                gap: None,
                align_items: None,
                justify_content: None,
                wrap: false,
                event_id: None,
            })
        });
        assert!(
            ViewTree::new(deep_view)
                .validate(ViewLimits {
                    max_depth: 3,
                    ..ViewLimits::default()
                })
                .is_err()
        );
    }

    #[test]
    fn manifest_schema_fixture_matches_the_contract_owner() {
        let schema_str =
            ExtensionManifest::schema_json().expect("manifest schema should serialize");
        let schema = serde_json::from_str::<serde_json::Value>(&schema_str)
            .expect("generated schema should be valid JSON");
        let fixture_str = include_str!("../schema/extension-v1.schema.json");
        let fixture = serde_json::from_str::<serde_json::Value>(fixture_str)
            .expect("checked-in schema should be valid JSON");
        assert_eq!(schema, fixture);

        // Explicit new/legacy name assertions.
        let legacy_provider_field = ["launcher", "providers"].join("_");
        let legacy_provider_type = ["Launcher", "ProviderContribution"].concat();
        assert!(schema_str.contains("search_providers"));
        assert!(schema_str.contains("SearchProviderContribution"));
        assert!(!schema_str.contains(&legacy_provider_field));
        assert!(!schema_str.contains(&legacy_provider_type));
        assert!(schema_str.contains("wallpaper_providers"));
        assert!(schema_str.contains("WallpaperProviderContribution"));

        assert!(fixture_str.contains("search_providers"));
        assert!(fixture_str.contains("SearchProviderContribution"));
        assert!(!fixture_str.contains(&legacy_provider_field));
        assert!(!fixture_str.contains(&legacy_provider_type));
        assert!(fixture_str.contains("wallpaper_providers"));
        assert!(fixture_str.contains("WallpaperProviderContribution"));
    }

    #[test]
    fn wit_package_resolves_without_errors() {
        let mut resolve = wit_parser::Resolve::default();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("wit");
        let (pkg_id, _) = resolve
            .push_dir(&path)
            .expect("WIT package directory must resolve");
        let pkg = &resolve.packages[pkg_id];
        assert_eq!(pkg.name.namespace, "shilpo");
        assert_eq!(pkg.name.name, "extension");
        assert_eq!(
            pkg.name.version,
            Some(semver::Version::parse("0.1.0").unwrap())
        );
    }

    #[test]
    fn secret_purpose_validation_accepts_valid_and_rejects_invalid() {
        let valid = [
            "weather-api",
            "oauth-refresh",
            "a",
            "a-1-b",
            "a0123456789-b",
        ];
        for s in valid {
            assert!(
                SecretPurpose::parse(s).is_ok(),
                "should accept valid purpose '{s}'"
            );
        }

        let long_str = "a".repeat(65);
        let invalid = [
            "",
            "Weather-api",
            "weather_api",
            "weather/api",
            "-weather",
            "1weather",
            "weather api",
            "weather.api",
            long_str.as_str(),
        ];
        for s in invalid {
            assert!(
                SecretPurpose::parse(s).is_err(),
                "should reject invalid purpose '{s}'"
            );
        }
    }

    #[test]
    fn secrets_capability_validates_purposes_and_rejects_duplicates() {
        let manifest_toml = r#"
            id = "io.github.test.secrets"
            name = "Secrets Test"
            version = "1.0.0"
            schema_version = 1
            api_version = "0.1.0"

            [[capabilities]]
            kind = "secrets"
            purposes = ["api-key", "refresh-token"]
        "#;
        let manifest = ExtensionManifest::from_toml(manifest_toml).unwrap();
        assert_eq!(
            manifest.capabilities,
            vec![Capability::Secrets {
                purposes: vec![
                    SecretPurpose::parse("api-key").unwrap(),
                    SecretPurpose::parse("refresh-token").unwrap(),
                ]
            }]
        );

        let duplicate_toml = r#"
            id = "io.github.test.secrets"
            name = "Secrets Test"
            version = "1.0.0"
            schema_version = 1
            api_version = "0.1.0"

            [[capabilities]]
            kind = "secrets"
            purposes = ["api-key", "api-key"]
        "#;
        assert!(ExtensionManifest::from_toml(duplicate_toml).is_err());
    }

    #[test]
    fn secret_ref_serializes_as_tagged_json_and_redacts_debug_output() {
        let reference = SecretRef::new("secret-handle-9999");
        let json = serde_json::to_string(&reference).expect("SecretRef should serialize");
        assert_eq!(json, r#"{"secret_ref":"secret-handle-9999"}"#);

        let deserialized: SecretRef =
            serde_json::from_str(&json).expect("SecretRef should deserialize");
        assert_eq!(deserialized.handle, "secret-handle-9999");

        let debug_str = format!("{reference:?}");
        assert!(
            !debug_str.contains("secret-handle-9999"),
            "Debug output must not leak handle"
        );
        assert!(debug_str.contains("<redacted>"));

        let display_str = format!("{reference}");
        assert!(
            !display_str.contains("secret-handle-9999"),
            "Display output must not leak handle"
        );
        assert!(display_str.contains("<redacted>"));
    }

    #[test]
    fn wallpaper_provider_validation_accepts_valid_and_rejects_invalid() {
        let valid_manifest = r#"
            id = "org.shilpo.wallpaper"
            name = "Wallpaper"
            version = "0.1.0"
            schema_version = 1
            api_version = "0.1.0"

            [[contributions.wallpaper_providers]]
            id = "provider"
            name = "Wallpaper Provider"
            modes = ["manual", "slideshow"]
            targets = ["global", "workspace"]
        "#;
        let manifest = ExtensionManifest::from_toml(valid_manifest).unwrap();
        assert_eq!(manifest.contributions.wallpaper_providers.len(), 1);
        let wp = &manifest.contributions.wallpaper_providers[0];
        assert_eq!(wp.id.as_str(), "provider");
        assert_eq!(
            wp.modes,
            vec![WallpaperMode::Manual, WallpaperMode::Slideshow]
        );
        assert_eq!(
            wp.targets,
            vec![WallpaperTargetKind::Global, WallpaperTargetKind::Workspace]
        );

        // Empty modes
        let empty_modes =
            valid_manifest.replace(r#"modes = ["manual", "slideshow"]"#, r#"modes = []"#);
        assert!(matches!(
            ExtensionManifest::from_toml(&empty_modes),
            Err(ManifestError::Validation(msg)) if msg.contains("at least one mode")
        ));

        // Duplicate modes
        let dup_modes = valid_manifest.replace(
            r#"modes = ["manual", "slideshow"]"#,
            r#"modes = ["manual", "manual"]"#,
        );
        assert!(matches!(
            ExtensionManifest::from_toml(&dup_modes),
            Err(ManifestError::Validation(msg)) if msg.contains("duplicate mode")
        ));

        // Empty targets
        let empty_targets =
            valid_manifest.replace(r#"targets = ["global", "workspace"]"#, r#"targets = []"#);
        assert!(matches!(
            ExtensionManifest::from_toml(&empty_targets),
            Err(ManifestError::Validation(msg)) if msg.contains("at least one target")
        ));

        // Duplicate targets
        let dup_targets = valid_manifest.replace(
            r#"targets = ["global", "workspace"]"#,
            r#"targets = ["global", "global"]"#,
        );
        assert!(matches!(
            ExtensionManifest::from_toml(&dup_targets),
            Err(ManifestError::Validation(msg)) if msg.contains("duplicate target")
        ));
    }
}
