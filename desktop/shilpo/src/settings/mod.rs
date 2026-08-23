use gpui::{
    App, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render,
    Styled, Window, div,
};
use shilpo_m3e::{
    ActiveTheme, IconName, NavigationRail, NavigationRailHeader, NavigationRailItem,
    NavigationRailMenuButton, Selectable, StyledExt, h_flex, v_flex,
};

pub mod keybindings_page;
pub mod quick_page;

/// Settings App Navigation Category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsCategory {
    #[default]
    Quick,
    Network,
    Bluetooth,
    Bar,
    Desktop,
    Interface,
    Keybindings,
    Storage,
}

impl SettingsCategory {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Quick => "Quick",
            Self::Network => "Network",
            Self::Bluetooth => "Bluetooth",
            Self::Bar => "Bar",
            Self::Desktop => "Desktop",
            Self::Interface => "Interface",
            Self::Keybindings => "Keybindings",
            Self::Storage => "Storage",
        }
    }

    pub fn icon(&self, active: bool) -> IconName {
        match self {
            Self::Quick => IconName::InstantMix,
            Self::Network => IconName::AndroidWifi3Bar,
            Self::Bluetooth => IconName::Bluetooth,
            Self::Bar => {
                if active {
                    IconName::ToolbarFill
                } else {
                    IconName::Toolbar
                }
            }
            Self::Desktop => {
                if active {
                    IconName::ComputerFill
                } else {
                    IconName::Computer
                }
            }
            Self::Interface => {
                if active {
                    IconName::BottomAppBarFill
                } else {
                    IconName::BottomAppBar
                }
            }
            Self::Keybindings => IconName::InstantMix,
            Self::Storage => IconName::Storage,
        }
    }

    pub const ALL: &'static [Self] = &[
        Self::Quick,
        Self::Network,
        Self::Bluetooth,
        Self::Bar,
        Self::Desktop,
        Self::Interface,
        Self::Keybindings,
        Self::Storage,
    ];
}

#[derive(Debug, Clone)]
pub struct SettingsPageDescriptor {
    pub category: SettingsCategory,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct SettingsPageRegistry {
    pages: Vec<SettingsPageDescriptor>,
}

impl SettingsPageRegistry {
    pub fn discover() -> Self {
        let pages = SettingsCategory::ALL
            .iter()
            .map(|category| SettingsPageDescriptor {
                category: *category,
                label: category.label().to_owned(),
            })
            .collect::<Vec<_>>();
        Self { pages }
    }

    pub fn pages(&self) -> &[SettingsPageDescriptor] {
        &self.pages
    }
}

/// Standalone Settings Application View.
pub struct SettingsView {
    pub active_category: SettingsCategory,
    pub page_registry: SettingsPageRegistry,
    pub rail_collapsed: bool,
    pub theme_client: shilpo_theme_daemon::ThemeClient,
    pub device_client: shilpo_services::DeviceClient,
    pub device_states:
        std::collections::HashMap<shilpo_services::DeviceDomain, shilpo_services::DomainState>,
}

impl SettingsView {
    pub fn new(theme_client: shilpo_theme_daemon::ThemeClient, cx: &mut Context<Self>) -> Self {
        let page_registry = SettingsPageRegistry::discover();
        let device_client = shilpo_services::DeviceClient::new();
        let mut device_updates = device_client.subscribe_updates();
        let device_client_task = device_client.clone();
        cx.spawn(async move |_this, _cx| {
            device_client_task.maintain_connection().await;
        })
        .detach();
        cx.spawn(async move |this, cx| {
            while let Ok(update) = device_updates.recv().await {
                if let Some(this) = this.upgrade() {
                    this.update(cx, |view, cx| {
                        view.device_states.insert(update.domain, update.state);
                        cx.notify();
                    });
                }
            }
        })
        .detach();

        // 1. Immediately apply current theme state to GPUI Theme
        let current_theme = theme_client.current_state();
        shilpo_m3e::Theme::global_mut(cx).apply_state(&current_theme);

        // 2. Subscribe to theme updates and notify SettingsView on change
        let mut rx = theme_client.subscribe();
        let client_clone = theme_client.clone();
        cx.spawn(async move |this, cx| {
            loop {
                let update = match rx.recv().await {
                    Ok(update) => update,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        client_clone.current_update()
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                let mut latest = update.state;
                while let Ok(newer) = rx.try_recv() {
                    latest = newer.state;
                }
                cx.update(|cx: &mut App| {
                    shilpo_m3e::Theme::global_mut(cx).apply_state(&latest);
                    cx.refresh_windows();
                });
                if let Some(this) = this.upgrade() {
                    this.update(cx, |_, cx| {
                        cx.notify();
                    });
                }
            }
        })
        .detach();

        Self {
            active_category: SettingsCategory::default(),
            page_registry,
            rail_collapsed: false,
            theme_client,
            device_client,
            device_states: shilpo_services::DeviceDomain::ALL
                .into_iter()
                .map(|domain| {
                    (
                        domain,
                        shilpo_services::DomainState {
                            domain,
                            version: shilpo_device::DomainVersion::ZERO,
                            lifecycle: shilpo_services::DomainLifecycle::Unavailable,
                            payload: shilpo_services::DomainPayload::empty(domain),
                            error: None,
                        },
                    )
                })
                .collect(),
        }
    }

    pub fn view(
        theme_client: shilpo_theme_daemon::ThemeClient,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<shilpo_m3e::Root> {
        #[cfg(target_os = "linux")]
        {
            register_desktop_entry();
            update_desktop_icon_for_theme(cx);
        }

        let view = cx.new(|cx| Self::new(theme_client, cx));
        cx.new(|cx| {
            shilpo_m3e::Root::new(view, window, cx)
                .bordered(true)
                .bg(cx.theme().surface)
        })
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.active_category;
        let active_label = active.label();

        h_flex()
            .size_full()
            .bg(cx.theme().surface)
            .text_color(cx.theme().on_surface)
            // Left Navigation Sidebar (M3 Expressive Navigation Rail)
            .child({
                let menu_button = NavigationRailMenuButton::new("rail-toggle")
                    .collapsed(self.rail_collapsed)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.rail_collapsed = !this.rail_collapsed;
                        cx.notify();
                    }));

                let rail_header = NavigationRailHeader::new("settings-rail-header").child(menu_button);

                let rail_items: Vec<_> = self
                    .page_registry
                    .pages()
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(index, page)| {
                        let is_active = active == page.category;
                        let icon = page.category.icon(is_active);
                        let category = page.category;
                        NavigationRailItem::new(("settings-page", index))
                            .icon(icon)
                            .label(page.label)
                            .selected(is_active)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.active_category = category;
                                cx.notify();
                            }))
                    })
                    .collect();

                NavigationRail::new("settings-nav-rail")
                    .collapsed(self.rail_collapsed)
                    .header(rail_header)
                    .items(rail_items)
            })
            // Main Content Area (Pocket Card UI matching Storybook gallery)
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .overflow_hidden()
                    .py_3()
                    .pr_3()
                    .pl_2()
                    .child(
                        v_flex()
                            .id(gpui::ElementId::Name(gpui::SharedString::from(format!(
                                "settings-page-content-{:?}",
                                active
                            ))))
                            .size_full()
                            .min_w_0()
                            .bg(cx.theme().surface_container_low)
                            .rounded_2xl()
                            .p_6()
                            .gap_4()
                            .child(
                                h_flex()
                                    .gap_3()
                                    .items_center()
                                    .child(
                                        shilpo_m3e::Icon::new(active.icon(true))
                                            .size(gpui::px(28.))
                                            .text_color(cx.theme().primary),
                                    )
                                    .child(
                                        div()
                                            .text_xl()
                                            .font_bold()
                                            .text_color(cx.theme().on_surface)
                                            .child(active_label),
                                    ),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().on_surface_variant)
                                    .child(format!("Configure {} settings and system preferences.", active_label)),
                            )
                            .child(if active == SettingsCategory::Quick {
                                quick_page::QuickPage::render(
                                    &self.theme_client,
                                    &self.device_client,
                                    &self.device_states,
                                    _window,
                                    cx,
                                )
                                    .into_any_element()
                            } else if active == SettingsCategory::Keybindings {
                                keybindings_page::KeybindingsPage::render(_window, cx).into_any_element()
                            } else {
                                v_flex()
                                    .flex_1()
                                    .items_center()
                                    .justify_center()
                                    .gap_3()
                                    .p_8()
                                    .rounded_xl()
                                    .border_1()
                                    .border_color(cx.theme().outline_variant)
                                    .bg(cx.theme().surface_container)
                                    .child(
                                        shilpo_m3e::Icon::new(active.icon(false))
                                            .size(gpui::px(48.))
                                            .text_color(cx.theme().on_surface_variant),
                                    )
                                    .child(
                                        div()
                                            .text_base()
                                            .font_semibold()
                                            .text_color(cx.theme().on_surface)
                                            .child(format!("{} Settings", active_label)),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().on_surface_variant)
                                            .child("Settings page options will be added here in future updates."),
                                    )
                                    .into_any_element()
                            }),
                    ),
            )
    }
}

#[cfg(target_os = "linux")]
pub fn register_desktop_entry() {
    if let Some(home) = dirs::home_dir() {
        let apps_dir = home.join(".local/share/applications");
        let icons_scalable_dir = home.join(".local/share/icons/hicolor/scalable/apps");

        let _ = std::fs::create_dir_all(&apps_dir);
        let _ = std::fs::create_dir_all(&icons_scalable_dir);

        let desktop_file = apps_dir.join("org.shilpo.settings.desktop");
        let icon_svg_file = icons_scalable_dir.join("org.shilpo.settings.svg");

        let desktop_content = include_str!("../../resources/org.shilpo.settings.desktop");
        let icon_svg_content = include_bytes!("../../resources/org.shilpo.settings.svg");

        let _ = std::fs::write(&desktop_file, desktop_content);
        let _ = std::fs::write(&icon_svg_file, icon_svg_content);
    }
}

#[cfg(target_os = "linux")]
pub fn update_desktop_icon_for_theme(cx: &App) {
    if let Some(home) = dirs::home_dir() {
        let icons_scalable_dir = home.join(".local/share/icons/hicolor/scalable/apps");
        let pixmaps_dir = home.join(".local/share/pixmaps");
        let _ = std::fs::create_dir_all(&icons_scalable_dir);
        let _ = std::fs::create_dir_all(&pixmaps_dir);

        let icon_svg_file = icons_scalable_dir.join("org.shilpo.settings.svg");

        let is_dark = cx.theme().mode.is_dark();
        let bg_hsla = if is_dark {
            cx.theme().surface_container_high
        } else {
            cx.theme().primary_container
        };
        let bg_rgb = bg_hsla.to_rgb();
        let bg_color = format!(
            "#{:02x}{:02x}{:02x}",
            (bg_rgb.r * 255.0) as u8,
            (bg_rgb.g * 255.0) as u8,
            (bg_rgb.b * 255.0) as u8
        );

        let primary_rgb = cx.theme().primary.to_rgb();
        let glyph_color = format!(
            "#{:02x}{:02x}{:02x}",
            (primary_rgb.r * 255.0) as u8,
            (primary_rgb.g * 255.0) as u8,
            (primary_rgb.b * 255.0) as u8
        );

        let svg_content = format!(
            r#"<svg width="512" height="512" viewBox="0 0 512 512" fill="none" xmlns="http://www.w3.org/2000/svg">
    <rect width="512" height="512" rx="160" fill="{bg_color}"/>
    <g transform="translate(64, 448) scale(0.4)">
        <path fill="{glyph_color}" d="M433-80q-27 0-46.5-18T363-142l-9-66q-13-5-24.5-12T307-235l-62 26q-25 11-50 2t-39-32l-47-82q-14-23-8-49t27-43l53-40q-1-7-1-13.5v-27q0-6.5 1-13.5l-53-40q-21-17-27-43t8-49l47-82q14-23 39-32t50 2l62 26q11-8 23-15t24-12l9-66q4-26 23.5-44t46.5-18h94q27 0 46.5 18t23.5 44l9 66q13 5 24.5 12t22.5 15l62-26q25-11 50-2t39 32l47 82q14-23 8 49t-27 43l-53 40q1 7 1 13.5v27q0 6.5-2 13.5l53 40q21 17 27 43t-8 49l-48 82q-14 23-39 32t-50-2l-60-26q-11 8-23 15t-24 12l-9 66q-4 26-23.5 44T527-80h-94Zm49-260q58 0 99-41t41-99q0-58-41-99t-99-41q-59 0-99.5 41T342-480q0 58 40.5 99t99.5 41Z"/>
    </g>
</svg>"#
        );

        let _ = std::fs::write(&icon_svg_file, &svg_content);

        cx.background_executor()
            .spawn(async move {
                for size in [512, 256, 128, 64, 48, 32] {
                    let size_dir =
                        home.join(format!(".local/share/icons/hicolor/{size}x{size}/apps"));
                    let _ = std::fs::create_dir_all(&size_dir);
                    let png_file = size_dir.join("org.shilpo.settings.png");
                    let _ = std::process::Command::new("rsvg-convert")
                        .args([
                            "-w",
                            &size.to_string(),
                            "-h",
                            &size.to_string(),
                            icon_svg_file.to_str().unwrap(),
                            "-o",
                            png_file.to_str().unwrap(),
                        ])
                        .status();
                }

                let pixmap_png = pixmaps_dir.join("org.shilpo.settings.png");
                let _ = std::process::Command::new("rsvg-convert")
                    .args([
                        "-w",
                        "512",
                        "-h",
                        "512",
                        icon_svg_file.to_str().unwrap(),
                        "-o",
                        pixmap_png.to_str().unwrap(),
                    ])
                    .status();

                let _ = std::process::Command::new("gtk-update-icon-cache")
                    .args([
                        "-f",
                        "-t",
                        home.join(".local/share/icons/hicolor").to_str().unwrap(),
                    ])
                    .status();
            })
            .detach();
    }
}

fn single_instance_socket_path() -> std::path::PathBuf {
    dirs::runtime_dir()
        .unwrap_or_else(crate::config::cache_dir)
        .join("shilpo-settings.sock")
}

fn focus_settings_window_in_compositor() {
    let compositor = shilpo_services::init_compositor();
    let mut rx = compositor.subscribe();

    let try_focus = |snapshot: &shilpo_services::CompositorSnapshot| -> bool {
        if let Some(settings_win) = snapshot.windows.iter().find(|w| {
            w.app_id.as_deref() == Some("org.shilpo.settings")
                || w.app_id.as_deref() == Some("shilpo-settings")
        }) {
            let _ = compositor.command_broker().submit(
                shilpo_services::CompositorCommand::FocusWindow(settings_win.id),
            );
            true
        } else {
            false
        }
    };

    if try_focus(&rx.borrow_and_update()) {
        return;
    }

    for _ in 0..10 {
        std::thread::sleep(std::time::Duration::from_millis(25));
        if rx.has_changed().unwrap_or(false) && try_focus(&rx.borrow_and_update()) {
            break;
        }
    }
}

fn try_activate_existing_instance() -> bool {
    use std::io::Write;
    let socket_path = single_instance_socket_path();
    if let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&socket_path) {
        let _ = stream.write_all(b"focus\n");
        focus_settings_window_in_compositor();
        return true;
    }
    false
}

pub async fn run_settings() {
    use gpui::{Bounds, WindowBounds, WindowKind, WindowOptions, point, px, size};

    if try_activate_existing_instance() {
        println!("Shilpo Settings is already running. Focused existing window.");
        return;
    }

    let socket_path = single_instance_socket_path();
    let _ = std::fs::remove_file(&socket_path);
    let listener = std::os::unix::net::UnixListener::bind(&socket_path).ok();

    let theme_client = shilpo_theme_daemon::ThemeClient::new().await;
    let initial_theme_state = theme_client.current_state();
    let config_path = crate::config::default_config_path();
    let initial_config = crate::config::ConfigResolver::from_primary_path(&config_path)
        .resolve_initial()
        .map(|(snapshot, _)| snapshot.config)
        .unwrap_or_default();

    let app = gpui_platform::application().with_assets(crate::Assets);

    app.run(move |cx: &mut App| {
        shilpo_m3e::init(cx);
        crate::locale::ApplicationLocale::install(initial_config.locale.as_deref(), cx);

        shilpo_m3e::Theme::global_mut(cx).apply_state(&initial_theme_state);

        let (mut config_rx, config_task) = crate::shell::bar::service_worker::spawn_config_updates(
            cx.background_executor().clone(),
            config_path.clone(),
        );
        config_task.detach();
        cx.spawn(async move |cx| {
            while let Some(update) = config_rx.recv().await {
                if let crate::shell::bar::service_worker::ConfigUpdate::Loaded {
                    config,
                    changeset,
                } = update
                    && changeset.locale
                {
                    let locale = config.locale.clone();
                    cx.update(|cx| {
                        crate::locale::ApplicationLocale::apply_config(locale.as_deref(), cx);
                    });
                }
            }
        })
        .detach();

        cx.activate(true);

        let display_bounds = cx
            .primary_display()
            .map(|d| d.bounds())
            .unwrap_or_else(|| Bounds::new(point(px(0.), px(0.)), size(px(1920.), px(1080.))));

        let width = px(900.);
        let height = px(640.);
        let origin = point(
            display_bounds.origin.x + (display_bounds.size.width - width) / 2.0,
            display_bounds.origin.y + (display_bounds.size.height - height) / 2.0,
        );

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                origin,
                size(width, height),
            ))),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("Settings".into()),
                appears_transparent: false,
                traffic_light_position: None,
            }),
            kind: WindowKind::Normal,
            display_id: cx.primary_display().map(|d| d.id()),
            window_background: gpui::WindowBackgroundAppearance::Opaque,
            app_id: Some("org.shilpo.settings".into()),
            ..Default::default()
        };

        // Pass the shared ThemeClient so SettingsView uses the same instance.
        let tc = theme_client.clone();
        let window_handle = match cx.open_window(options, move |window, cx| {
            SettingsView::view(tc, window, cx)
        }) {
            Ok(handle) => handle,
            Err(err) => {
                eprintln!("Failed to open Settings window: {}", err);
                return;
            }
        };

        if let Some(listener) = listener {
            listener.set_nonblocking(true).ok();
            cx.spawn(async move |cx| {
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                    if let Ok((_stream, _)) = listener.accept() {
                        focus_settings_window_in_compositor();
                        cx.update(|cx| {
                            cx.activate(true);
                            let _ = window_handle.update(cx, |_, _, cx| cx.notify());
                            cx.refresh_windows();
                        });
                    }
                }
            })
            .detach();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_settings_categories() {
        assert_eq!(SettingsCategory::ALL.len(), 8);
        assert_eq!(SettingsCategory::Quick.label(), "Quick");
        assert_eq!(SettingsCategory::Network.label(), "Network");
        assert_eq!(SettingsCategory::Bluetooth.label(), "Bluetooth");
        assert_eq!(SettingsCategory::Bar.label(), "Bar");
        assert_eq!(SettingsCategory::Keybindings.label(), "Keybindings");
        assert_eq!(SettingsCategory::Desktop.label(), "Desktop");
        assert_eq!(SettingsCategory::Interface.label(), "Interface");
        assert_eq!(SettingsCategory::Storage.label(), "Storage");
    }
}
