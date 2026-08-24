use std::fs;
use std::path::PathBuf;

use gpui::{App, IntoElement, ParentElement, Styled, Window, div};
use shilpo_m3e::scroll::ScrollableElement;
use shilpo_m3e::{ActiveTheme, IconName, StyledExt, button::Button, h_flex, v_flex};

pub struct KeybindingsPage;

impl KeybindingsPage {
    pub fn render(_window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();

        v_flex()
            .flex_1()
            .w_full()
            .gap_4()
            .overflow_y_scrollbar()
            // Page Header
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .items_center()
                    .child(
                        v_flex()
                            .child(
                                div()
                                    .text_xl()
                                    .font_bold()
                                    .text_color(theme.on_surface)
                                    .child("Keyboard Shortcuts"),
                            )
                            .child(div().text_sm().text_color(theme.on_surface_variant).child(
                                "Configure global hotkeys for shell actions and extensions",
                            )),
                    )
                    .child(
                        Button::new("reset_all")
                            .label("Reset All to Defaults")
                            .icon(IconName::Refresh)
                            .on_click(|_, _, _| {
                                let _ = reset_all_keybinding_overrides();
                            }),
                    ),
            )
            .child(Self::render_resolved_section(cx))
        /* .child(Self::render_category_section(
            "Shell Actions",
            &[
                (
                    "builtin:toggle_overview",
                    "Toggle Workspace Overview",
                    "Super+Space",
                ),
                ("builtin:toggle_bar", "Toggle Desktop Bar", "Super+B"),
                (
                    "builtin:reload_config",
                    "Reload Shell Configuration",
                    "Super+Shift+R",
                ),
                ("builtin:quit", "Quit Shell Runtime", "Super+Shift+Q"),
            ],
            cx,
        ))
        .child(Self::render_category_section(
            "Window Management",
            &[
                ("builtin:focus_window", "Focus Window by ID", "Unbound"),
                (
                    "builtin:focus_previous_window",
                    "Focus Previous Window",
                    "Unbound",
                ),
                ("builtin:close_window", "Close Window", "Unbound"),
                (
                    "builtin:move_window_to_workspace",
                    "Move Window to Workspace",
                    "Unbound",
                ),
            ],
            cx,
        ))
        .child(Self::render_category_section(
            "Workspace Navigation",
            &[
                (
                    "builtin:focus_workspace",
                    "Focus Workspace by ID",
                    "Unbound",
                ),
                (
                    "builtin:create_workspace",
                    "Create New Workspace",
                    "Unbound",
                ),
            ],
            cx,
        ))
        .child(Self::render_category_section(
            "Media & Display",
            &[
                ("builtin:volume_up", "Increase Volume", "Unbound"),
                ("builtin:volume_down", "Decrease Volume", "Unbound"),
                ("builtin:volume_mute", "Toggle Mute", "Unbound"),
                ("builtin:brightness_up", "Increase Brightness", "Unbound"),
                ("builtin:brightness_down", "Decrease Brightness", "Unbound"),
                ("builtin:take_screenshot", "Take Screenshot", "Unbound"),
            ],
            cx,
        )) */
    }

    fn render_resolved_section(cx: &App) -> impl IntoElement {
        let resolved = crate::shell::runtime::ShellRuntime::resolved_shortcuts(cx);
        let descriptors = crate::shell::runtime::ShellRuntime::action_descriptors(cx);
        let theme = cx.theme();
        v_flex()
            .w_full()
            .gap_2()
            .child(
                div()
                    .text_base()
                    .font_bold()
                    .text_color(theme.on_surface)
                    .child("Resolved Actions"),
            )
            .children(descriptors.into_iter().map(|descriptor| {
                let id = descriptor.id.to_string();
                let binding = resolved
                    .iter()
                    .find(|item| item.action_id == descriptor.id)
                    .map(|item| format!("{} ({:?})", item.shortcut.to_spec(), item.origin))
                    .unwrap_or_else(|| {
                        if descriptor.id.to_string().starts_with("ext:") {
                            "Dormant/unavailable (ExtensionDefault)".into()
                        } else {
                            "Unbound (BuiltinDefault)".into()
                        }
                    });
                let reset_id = id.clone();
                let bind_id = id.clone();
                let disable_id = id.clone();
                h_flex()
                    .w_full()
                    .justify_between()
                    .items_center()
                    .p_3()
                    .rounded_lg()
                    .bg(theme.surface_container)
                    .child(
                        v_flex()
                            .child(
                                div()
                                    .font_semibold()
                                    .text_color(theme.on_surface)
                                    .child(descriptor.label),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.on_surface_variant)
                                    .child(id.clone()),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .px_3()
                                    .py_1()
                                    .rounded_full()
                                    .bg(theme.surface_container_highest)
                                    .text_xs()
                                    .text_color(theme.on_surface)
                                    .child(binding),
                            )
                            .child(
                                Button::new(format!("reset_{reset_id}"))
                                    .label("Reset")
                                    .icon(IconName::Refresh)
                                    .on_click(move |_, _, _| {
                                        let _ = reset_keybinding_override(&reset_id);
                                    }),
                            )
                            .child(
                                Button::new(format!("bind_{bind_id}"))
                                    .label("Bind Super+Shift+K")
                                    .on_click(move |_, _, _| {
                                        let _ = save_keybinding_override(
                                            &bind_id,
                                            Some("Super+Shift+K"),
                                            true,
                                        );
                                    }),
                            )
                            .child(
                                Button::new(format!("disable_{disable_id}"))
                                    .label("Disable")
                                    .on_click({
                                        move |_, _, _| {
                                            let _ =
                                                save_keybinding_override(&disable_id, None, false);
                                        }
                                    }),
                            ),
                    )
            }))
    }
}

pub fn user_config_path() -> PathBuf {
    crate::config::default_config_path()
}

pub fn save_keybinding_override(
    action_id: &str,
    shortcut: Option<&str>,
    enabled: bool,
) -> Result<(), String> {
    let primary = user_config_path();
    let overrides = primary.with_file_name("overrides.toml");
    let text = if overrides.exists() {
        fs::read_to_string(&overrides).map_err(|e| e.to_string())?
    } else {
        String::new()
    };

    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| e.to_string())?;

    let keybindings_item = doc.entry("keybindings").or_insert_with(|| {
        toml_edit::Item::Value(toml_edit::Value::Array(toml_edit::Array::new()))
    });

    if let toml_edit::Item::Value(toml_edit::Value::Array(arr)) = keybindings_item {
        let mut found_idx = None;
        for (i, val) in arr.iter().enumerate() {
            if let toml_edit::Value::InlineTable(tbl) = val
                && tbl.get("action").and_then(|v| v.as_str()) == Some(action_id)
            {
                found_idx = Some(i);
                break;
            }
        }

        let mut tbl = toml_edit::InlineTable::new();
        tbl.insert("action", action_id.into());
        if let Some(s) = shortcut {
            tbl.insert("shortcut", s.into());
        }
        tbl.insert("enabled", enabled.into());

        if let Some(idx) = found_idx {
            arr.replace(idx, tbl);
        } else {
            arr.push(tbl);
        }
    } else if let toml_edit::Item::ArrayOfTables(arr) = keybindings_item {
        let mut found_tbl = None;
        for tbl in arr.iter_mut() {
            if tbl.get("action").and_then(|v| v.as_str()) == Some(action_id) {
                found_tbl = Some(tbl);
                break;
            }
        }

        if let Some(tbl) = found_tbl {
            if let Some(s) = shortcut {
                tbl.insert("shortcut", toml_edit::value(s));
            } else {
                tbl.remove("shortcut");
            }
            tbl.insert("enabled", toml_edit::value(enabled));
        } else {
            let mut tbl = toml_edit::Table::new();
            tbl.insert("action", toml_edit::value(action_id));
            if let Some(s) = shortcut {
                tbl.insert("shortcut", toml_edit::value(s));
            }
            tbl.insert("enabled", toml_edit::value(enabled));
            arr.push(tbl);
        }
    }

    let array = doc
        .get("keybindings")
        .and_then(|item| item.as_value())
        .and_then(|value| value.as_array())
        .cloned()
        .ok_or_else(|| "keybindings must be an array".to_string())?;
    crate::config::ConfigOverrideService::for_primary_path(primary)
        .apply_batch(&[crate::config::OverrideEdit::set(
            ["keybindings"],
            toml_edit::Value::Array(array),
        )])
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub fn reset_keybinding_override(action_id: &str) -> Result<(), String> {
    let primary = user_config_path();
    let path = primary.with_file_name("overrides.toml");
    if !path.exists() {
        return Ok(());
    }
    let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| e.to_string())?;

    if let Some(toml_edit::Item::Value(toml_edit::Value::Array(arr))) = doc.get_mut("keybindings") {
        let mut remove_idx = None;
        for (i, val) in arr.iter().enumerate() {
            if let toml_edit::Value::InlineTable(tbl) = val
                && tbl.get("action").and_then(|v| v.as_str()) == Some(action_id)
            {
                remove_idx = Some(i);
                break;
            }
        }
        if let Some(idx) = remove_idx {
            arr.remove(idx);
        }
    } else if let Some(toml_edit::Item::ArrayOfTables(arr)) = doc.get_mut("keybindings") {
        let mut remove_idx = None;
        for (i, tbl) in arr.iter().enumerate() {
            if tbl.get("action").and_then(|v| v.as_str()) == Some(action_id) {
                remove_idx = Some(i);
                break;
            }
        }
        if let Some(idx) = remove_idx {
            arr.remove(idx);
        }
    }

    let array = doc
        .get("keybindings")
        .and_then(|item| item.as_value())
        .and_then(|value| value.as_array())
        .cloned();
    let service = crate::config::ConfigOverrideService::for_primary_path(primary);
    match array {
        Some(array) => service
            .apply_batch(&[crate::config::OverrideEdit::set(
                ["keybindings"],
                toml_edit::Value::Array(array),
            )])
            .map(|_| ())
            .map_err(|e| e.to_string()),
        None => service
            .apply_batch(&[crate::config::OverrideEdit::remove(["keybindings"])])
            .map(|_| ())
            .map_err(|e| e.to_string()),
    }
}

pub fn reset_all_keybinding_overrides() -> Result<(), String> {
    let primary = user_config_path();
    let path = primary.with_file_name("overrides.toml");
    if !path.exists() {
        return Ok(());
    }
    let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| e.to_string())?;

    doc.remove("keybindings");

    crate::config::ConfigOverrideService::for_primary_path(primary)
        .apply_batch(&[crate::config::OverrideEdit::remove(["keybindings"])])
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comment_preserving_keybinding_override_edits() {
        let initial_toml = r#"# User configuration header comment
version = 1

# Existing theme setting
[theme]
font_family = "sans-serif"
"#;
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("config.toml");
        fs::write(&config_path, initial_toml).unwrap();

        // Update single keybinding override
        let text = fs::read_to_string(&config_path).unwrap();
        let mut doc: toml_edit::DocumentMut = text.parse().unwrap();

        let keybindings_item = doc.entry("keybindings").or_insert_with(|| {
            toml_edit::Item::Value(toml_edit::Value::Array(toml_edit::Array::new()))
        });

        if let toml_edit::Item::Value(toml_edit::Value::Array(arr)) = keybindings_item {
            let mut tbl = toml_edit::InlineTable::new();
            tbl.insert("action", "builtin:toggle_overview".into());
            tbl.insert("shortcut", "Super+A".into());
            tbl.insert("enabled", true.into());
            arr.push(tbl);
        }

        fs::write(&config_path, doc.to_string()).unwrap();
        let updated = fs::read_to_string(&config_path).unwrap();

        assert!(updated.contains("# User configuration header comment"));
        assert!(updated.contains("# Existing theme setting"));
        assert!(updated.contains("builtin:toggle_overview"));
    }
}
