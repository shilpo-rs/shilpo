use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticStatus {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticItem {
    pub category: String,
    pub name: String,
    pub status: DiagnosticStatus,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repair_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit_identifier: Option<String>,
    pub fix_applied: bool,
}

#[derive(Debug, Clone, Default)]
pub struct DoctorChecker;

impl DoctorChecker {
    pub fn new() -> Self {
        Self
    }

    pub fn default_config_path() -> PathBuf {
        crate::config::default_config_path()
    }

    pub fn run_diagnostics(&self, auto_fix: bool) -> Vec<DiagnosticItem> {
        vec![
            self.check_niri_compositor(),
            self.check_systemd_user_units(),
            self.check_dbus_theme(),
            self.check_desktop_services(),
            self.check_gpu_vulkan(),
            self.check_wallpaper_daemon(auto_fix),
            self.check_niri_bindings(),
            self.check_config_file(auto_fix),
            self.check_terminal_fonts_cursors(),
            self.check_i2c_permissions(),
            self.check_xdg_user_dirs(auto_fix),
            self.check_capture(),
            self.check_polkit_agent(),
            self.check_idle_management(),
            self.check_lock_screen(),
        ]
    }

    pub fn run_first_login_report(&self, auto_fix: bool) -> (Vec<DiagnosticItem>, bool) {
        let items = self.run_diagnostics(auto_fix);
        let has_fail = items.iter().any(|i| i.status == DiagnosticStatus::Fail);

        let state_dir = crate::config::state_dir();
        let _ = std::fs::create_dir_all(&state_dir);

        let json_path = crate::config::doctor_report_json_path();
        let txt_path = crate::config::doctor_report_txt_path();
        let marker_path = crate::config::doctor_first_login_marker_path();

        if let Ok(json) = serde_json::to_string_pretty(&items) {
            let _ = std::fs::write(&json_path, json);
        }
        let report_txt = self.format_report(&items);
        let _ = std::fs::write(&txt_path, &report_txt);

        let title = if has_fail {
            "Shilpo Desktop Diagnostics: Issues Found"
        } else {
            "Shilpo Desktop Readiness Check: Passed"
        };
        let body = if has_fail {
            "Some desktop services or dependencies require attention. View report at ~/.local/state/shilpo/doctor-report.txt"
        } else {
            "All core desktop features, GPU drivers, and session units are operational."
        };

        let _ = std::process::Command::new("notify-send")
            .arg(title)
            .arg(body)
            .status();

        let timestamp = chrono::Utc::now().to_rfc3339();
        let _ = std::fs::write(&marker_path, format!("completed_at={timestamp}\n"));

        (items, has_fail)
    }

    pub fn check_niri_compositor(&self) -> DiagnosticItem {
        let socket_active = shilpo_services::compositor::niri::resolve_niri_socket_path()
            .map(|p| p.exists())
            .unwrap_or(false)
            || std::env::var("WAYLAND_DISPLAY").is_ok()
            || std::process::Command::new("pgrep")
                .arg("niri")
                .output()
                .is_ok_and(|o| o.status.success());

        if socket_active {
            DiagnosticItem {
                category: "Compositor".into(),
                name: "Niri Session & IPC Socket".into(),
                status: DiagnosticStatus::Pass,
                message: "Active Niri Wayland session and socket detected".into(),
                repair_command: None,
                unit_identifier: None,
                fix_applied: false,
            }
        } else {
            DiagnosticItem {
                category: "Compositor".into(),
                name: "Niri Session & IPC Socket".into(),
                status: DiagnosticStatus::Warn,
                message: "Niri session is not currently running (offline mode)".into(),
                repair_command: Some("niri".into()),
                unit_identifier: None,
                fix_applied: false,
            }
        }
    }

    pub fn check_systemd_user_units(&self) -> DiagnosticItem {
        let config_home = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::var("HOME")
                    .map(|h| PathBuf::from(h).join(".config"))
                    .unwrap_or_else(|_| PathBuf::from(".config"))
            });
        let wants_dir = config_home.join("systemd/user/niri.service.wants");

        let required_units = [
            "shilpo-shell.service",
            "shilpo-themed.service",
            "shilpo-wallpaper.service",
            "shilpo-polkit-agent.service",
            "shilpo-network-agent.service",
            "shilpo-keyring.service",
            "shilpo-swayidle.service",
            "shilpo-first-login.service",
        ];

        let missing: Vec<&str> = required_units
            .iter()
            .copied()
            .filter(|u| !wants_dir.join(u).exists())
            .collect();

        let failed_output = std::process::Command::new("systemctl")
            .args(["--user", "--failed", "--no-legend"])
            .output();

        let has_failed_units = failed_output
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("shilpo"))
            .unwrap_or(false);
        let inactive_units: Vec<&str> = required_units
            .iter()
            .copied()
            .filter(|unit| {
                let active = std::process::Command::new("systemctl")
                    .args(["--user", "is-active", "--quiet", unit])
                    .status()
                    .is_ok_and(|status| status.success());
                if active {
                    return false;
                }

                // `shilpo-first-login.service` is an intentional oneshot.
                // After it completes successfully systemd reports it as
                // inactive (or keeps it inactive when the marker exists).
                // That is healthy and must not make doctor fail every login.
                if *unit == "shilpo-first-login.service" {
                    if crate::config::doctor_first_login_marker_path().exists() {
                        return false;
                    }
                    return !std::process::Command::new("systemctl")
                        .args(["--user", "show", "--property=Result", "--value", unit])
                        .output()
                        .is_ok_and(|output| {
                            output.status.success()
                                && String::from_utf8_lossy(&output.stdout).trim() == "success"
                        });
                }
                true
            })
            .collect();

        if !missing.is_empty() {
            DiagnosticItem {
                category: "Systemd".into(),
                name: "Niri Session Wants Links".into(),
                status: DiagnosticStatus::Fail,
                message: format!("Missing niri.service.wants links: {}", missing.join(", ")),
                repair_command: Some("./setup update".into()),
                unit_identifier: Some("niri.service".into()),
                fix_applied: false,
            }
        } else if has_failed_units || !inactive_units.is_empty() {
            DiagnosticItem {
                category: "Systemd".into(),
                name: "User Unit Status".into(),
                status: DiagnosticStatus::Fail,
                message: if has_failed_units {
                    "One or more Shilpo user units are in a failed state".into()
                } else {
                    format!("Inactive Shilpo user units: {}", inactive_units.join(", "))
                },
                repair_command: Some(
                    "systemctl --user reset-failed && systemctl --user restart niri.service.wants/*".into(),
                ),
                unit_identifier: Some("shilpo-shell.service".into()),
                fix_applied: false,
            }
        } else {
            DiagnosticItem {
                category: "Systemd".into(),
                name: "Session Services & Wants Links".into(),
                status: DiagnosticStatus::Pass,
                message: "All Shilpo user units are wired into niri.service.wants and operational"
                    .into(),
                repair_command: None,
                unit_identifier: None,
                fix_applied: false,
            }
        }
    }

    pub fn check_dbus_theme(&self) -> DiagnosticItem {
        let data_home = std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::var("HOME")
                    .map(|h| PathBuf::from(h).join(".local/share"))
                    .unwrap_or_else(|_| PathBuf::from(".local/share"))
            });
        let dbus_service = data_home.join("dbus-1/services/org.shilpo.Theme.service");

        if dbus_service.exists()
            || PathBuf::from("/usr/share/dbus-1/services/org.shilpo.Theme.service").exists()
        {
            DiagnosticItem {
                category: "D-Bus".into(),
                name: "Theme Activation Service".into(),
                status: DiagnosticStatus::Pass,
                message: "org.shilpo.Theme D-Bus service file detected".into(),
                repair_command: None,
                unit_identifier: None,
                fix_applied: false,
            }
        } else {
            DiagnosticItem {
                category: "D-Bus".into(),
                name: "Theme Activation Service".into(),
                status: DiagnosticStatus::Fail,
                message: "org.shilpo.Theme.service is missing from D-Bus service directory".into(),
                repair_command: Some("./setup update".into()),
                unit_identifier: Some("org.shilpo.Theme.service".into()),
                fix_applied: false,
            }
        }
    }

    pub fn check_desktop_services(&self) -> DiagnosticItem {
        let nm_ok = std::process::Command::new("nmcli")
            .arg("general")
            .output()
            .is_ok_and(|o| o.status.success())
            || std::process::Command::new("systemctl")
                .args(["is-active", "NetworkManager.service"])
                .output()
                .is_ok_and(|o| o.status.success());

        let bt_ok = is_bluez_dbus_active();

        let audio_ok = std::process::Command::new("wpctl")
            .arg("status")
            .output()
            .is_ok_and(|o| o.status.success())
            || std::process::Command::new("pactl")
                .arg("info")
                .output()
                .is_ok_and(|o| o.status.success());

        let dm_ok = std::process::Command::new("systemctl")
            .args(["is-enabled", "sddm.service"])
            .output()
            .is_ok_and(|o| o.status.success())
            || std::process::Command::new("systemctl")
                .args(["is-enabled", "display-manager.service"])
                .output()
                .is_ok_and(|o| o.status.success());

        let mut issues = Vec::new();
        if !nm_ok {
            issues.push("NetworkManager");
        }
        if !bt_ok {
            issues.push("Bluetooth");
        }
        if !audio_ok {
            issues.push("PipeWire/WirePlumber");
        }
        if !dm_ok {
            issues.push("SDDM/DisplayManager");
        }

        if issues.is_empty() {
            DiagnosticItem {
                category: "Desktop Services".into(),
                name: "Network, Bluetooth, Audio & DM".into(),
                status: DiagnosticStatus::Pass,
                message: "NetworkManager, Bluetooth, PipeWire, and SDDM are operational".into(),
                repair_command: None,
                unit_identifier: None,
                fix_applied: false,
            }
        } else {
            DiagnosticItem {
                category: "Desktop Services".into(),
                name: "Network, Bluetooth, Audio & DM".into(),
                status: DiagnosticStatus::Warn,
                message: format!(
                    "Inactive or unverified desktop services: {}",
                    issues.join(", ")
                ),
                repair_command: Some(
                    "sudo systemctl enable --now NetworkManager bluetooth sddm".into(),
                ),
                unit_identifier: Some("sddm.service".into()),
                fix_applied: false,
            }
        }
    }

    pub fn check_gpu_vulkan(&self) -> DiagnosticItem {
        let vulkan_ok = std::process::Command::new("vulkaninfo")
            .arg("--summary")
            .output()
            .is_ok_and(|o| o.status.success());

        if vulkan_ok {
            DiagnosticItem {
                category: "Graphics & GPU".into(),
                name: "Vulkan Driver Provider".into(),
                status: DiagnosticStatus::Pass,
                message: "Usable Vulkan ICD loader and GPU driver active".into(),
                repair_command: None,
                unit_identifier: None,
                fix_applied: false,
            }
        } else {
            DiagnosticItem {
                category: "Graphics & GPU".into(),
                name: "Vulkan Driver Provider".into(),
                status: DiagnosticStatus::Fail,
                message: "No active Vulkan driver found; run package resolution".into(),
                repair_command: Some(
                    "sudo pacman -S vulkan-tools mesa vulkan-intel vulkan-radeon nvidia-utils"
                        .into(),
                ),
                unit_identifier: None,
                fix_applied: false,
            }
        }
    }

    pub fn check_wallpaper_daemon(&self, auto_fix: bool) -> DiagnosticItem {
        let awww_ok = std::process::Command::new("awww")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());

        let wall_dir = crate::config::ShellConfig::load(Self::default_config_path())
            .map(|config| expand_home_path(config.desktop.wallpaper_dir))
            .unwrap_or_else(|_| expand_home_path(PathBuf::from("~/Pictures/Wallpapers")));
        if !wall_dir.exists() && auto_fix {
            let _ = std::fs::create_dir_all(&wall_dir);
        }
        let wall_exists = wall_dir.exists();

        if awww_ok && wall_exists {
            DiagnosticItem {
                category: "Wallpaper".into(),
                name: "awww Daemon & Wallpaper Directory".into(),
                status: DiagnosticStatus::Pass,
                message: format!(
                    "awww backend available and wallpapers folder ready at {}",
                    wall_dir.display()
                ),
                repair_command: None,
                unit_identifier: None,
                fix_applied: auto_fix,
            }
        } else if !awww_ok {
            DiagnosticItem {
                category: "Wallpaper".into(),
                name: "awww Daemon & Wallpaper Directory".into(),
                status: DiagnosticStatus::Warn,
                message: "awww binary not found on $PATH".into(),
                repair_command: Some("sudo pacman -S awww".into()),
                unit_identifier: Some("shilpo-wallpaper.service".into()),
                fix_applied: false,
            }
        } else {
            DiagnosticItem {
                category: "Wallpaper".into(),
                name: "awww Daemon & Wallpaper Directory".into(),
                status: DiagnosticStatus::Warn,
                message: format!("Wallpaper directory missing at {}", wall_dir.display()),
                repair_command: Some(format!("mkdir -p {}", wall_dir.display())),
                unit_identifier: None,
                fix_applied: false,
            }
        }
    }

    pub fn check_niri_bindings(&self) -> DiagnosticItem {
        let required_cmds = [
            "kitty",
            "nautilus",
            "swaylock",
            "swayidle",
            "brightnessctl",
            "playerctl",
            "wpctl",
        ];

        let missing: Vec<&str> = required_cmds
            .iter()
            .copied()
            .filter(|cmd| !is_command_available(cmd))
            .collect();

        if missing.is_empty() {
            DiagnosticItem {
                category: "Keybindings".into(),
                name: "Bound Executables Availability".into(),
                status: DiagnosticStatus::Pass,
                message: "All Niri keybinding helper binaries (kitty, nautilus, swaylock, etc.) are installed".into(),
                repair_command: None,
                unit_identifier: None,
                fix_applied: false,
            }
        } else {
            DiagnosticItem {
                category: "Keybindings".into(),
                name: "Bound Executables Availability".into(),
                status: DiagnosticStatus::Warn,
                message: format!("Missing keybinding binaries: {}", missing.join(", ")),
                repair_command: Some(format!("sudo pacman -S --needed {}", missing.join(" "))),
                unit_identifier: None,
                fix_applied: false,
            }
        }
    }

    pub fn check_config_file(&self, auto_fix: bool) -> DiagnosticItem {
        self.check_config_file_at(&Self::default_config_path(), auto_fix)
    }

    pub fn check_config_file_at(&self, config_path: &Path, auto_fix: bool) -> DiagnosticItem {
        if config_path.exists() {
            let resolver = crate::config::ConfigResolver::from_primary_path(config_path);
            match resolver.resolve_initial() {
                Ok((_snapshot, report)) => {
                    if report.unknown_keys.is_empty() {
                        DiagnosticItem {
                            category: "Configuration".into(),
                            name: "Shilpo config.toml".into(),
                            status: DiagnosticStatus::Pass,
                            message: format!("Valid configuration at {}", config_path.display()),
                            repair_command: None,
                            unit_identifier: None,
                            fix_applied: false,
                        }
                    } else {
                        let details = report
                            .unknown_keys
                            .iter()
                            .map(|key| key.describe())
                            .collect::<Vec<_>>()
                            .join("; ");
                        DiagnosticItem {
                            category: "Configuration".into(),
                            name: "Shilpo config.toml".into(),
                            status: DiagnosticStatus::Warn,
                            message: format!(
                                "Valid configuration with unknown keys at {}: {}",
                                config_path.display(),
                                details
                            ),
                            repair_command: None,
                            unit_identifier: None,
                            fix_applied: false,
                        }
                    }
                }
                Err(err) => DiagnosticItem {
                    category: "Configuration".into(),
                    name: "Shilpo config.toml".into(),
                    status: DiagnosticStatus::Fail,
                    message: format!("Configuration syntax/schema error: {err}"),
                    repair_command: Some("./setup update".into()),
                    unit_identifier: None,
                    fix_applied: false,
                },
            }
        } else if auto_fix {
            let _ = crate::config::ShellConfig::load_or_create(config_path);
            DiagnosticItem {
                category: "Configuration".into(),
                name: "Shilpo config.toml".into(),
                status: DiagnosticStatus::Pass,
                message: format!("Created default config file at {}", config_path.display()),
                repair_command: None,
                unit_identifier: None,
                fix_applied: true,
            }
        } else {
            DiagnosticItem {
                category: "Configuration".into(),
                name: "Shilpo config.toml".into(),
                status: DiagnosticStatus::Warn,
                message: format!(
                    "File missing at {}; defaults will be used",
                    config_path.display()
                ),
                repair_command: Some("./setup install".into()),
                unit_identifier: None,
                fix_applied: false,
            }
        }
    }

    pub fn check_terminal_fonts_cursors(&self) -> DiagnosticItem {
        let shell_ok = std::env::var("SHELL")
            .is_ok_and(|shell| shell == "/usr/bin/fish" || shell.ends_with("/fish"))
            || std::env::var("USER")
                .ok()
                .and_then(|user| {
                    std::process::Command::new("getent")
                        .args(["passwd", &user])
                        .output()
                        .ok()
                })
                .is_some_and(|output| {
                    output.status.success()
                        && String::from_utf8_lossy(&output.stdout)
                            .trim_end()
                            .ends_with(":/usr/bin/fish")
                });

        let font_ok = std::process::Command::new("fc-match")
            .arg("JetBrainsMono Nerd Font")
            .output()
            .is_ok_and(|o| {
                o.status.success() && String::from_utf8_lossy(&o.stdout).contains("JetBrains")
            });

        let cursor_ok = PathBuf::from("/usr/share/icons/capitaine-cursors").exists()
            || PathBuf::from("/usr/share/icons/breeze_cursors").exists();

        if shell_ok && font_ok && cursor_ok {
            DiagnosticItem {
                category: "Appearance".into(),
                name: "Shell, Fonts & Cursor Theme".into(),
                status: DiagnosticStatus::Pass,
                message:
                    "Fish shell, JetBrains Mono Nerd Font, and Capitaine cursor theme available"
                        .into(),
                repair_command: None,
                unit_identifier: None,
                fix_applied: false,
            }
        } else {
            DiagnosticItem {
                category: "Appearance".into(),
                name: "Shell, Fonts & Cursor Theme".into(),
                status: DiagnosticStatus::Warn,
                message: "One or more desktop theme assets (Fish, JetBrains Mono, Capitaine cursors) need installation"
                    .into(),
                repair_command: Some("sudo pacman -S fish ttf-jetbrains-mono-nerd capitaine-cursors".into()),
                unit_identifier: None,
                fix_applied: false,
            }
        }
    }

    pub fn check_i2c_permissions(&self) -> DiagnosticItem {
        let i2c_buses = shilpo_services::brightness::discover_i2c_bus_paths();
        let i2c_devs: Vec<PathBuf> = i2c_buses.into_iter().map(|(_, p)| p).collect();

        let udev_rule_exists = PathBuf::from("/etc/udev/rules.d/60-ddcutil.rules").exists()
            || PathBuf::from("/usr/lib/udev/rules.d/60-ddcutil.rules").exists()
            || PathBuf::from("/lib/udev/rules.d/60-ddcutil.rules").exists();

        let in_i2c_group = std::process::Command::new("id")
            .arg("-nG")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("i2c"))
            .unwrap_or(false);

        if i2c_devs.is_empty() {
            return DiagnosticItem {
                category: "Hardware".into(),
                name: "Linux I2C Dev Devices & Permissions".into(),
                status: DiagnosticStatus::Warn,
                message: "No /dev/i2c-* devices found. Ensure 'i2c-dev' module is loaded: sudo modprobe i2c-dev".into(),
                repair_command: Some("sudo modprobe i2c-dev".into()),
                unit_identifier: None,
                fix_applied: false,
            };
        }

        let unreadable: Vec<String> = i2c_devs
            .iter()
            .filter(|p| {
                std::fs::File::options()
                    .read(true)
                    .write(true)
                    .open(p)
                    .is_err()
            })
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();

        if !unreadable.is_empty() || !in_i2c_group || !udev_rule_exists {
            let mut issues = Vec::new();
            if !unreadable.is_empty() {
                issues.push(format!(
                    "restricted permissions on: {}",
                    unreadable.join(", ")
                ));
            }
            if !in_i2c_group {
                issues.push("user not in 'i2c' group".into());
            }
            if !udev_rule_exists {
                issues.push("60-ddcutil.rules missing".into());
            }

            let repair = if !in_i2c_group {
                "sudo usermod -aG i2c $USER && sudo udevadm control --reload-rules"
            } else {
                "sudo udevadm control --reload-rules && sudo udevadm trigger"
            };

            DiagnosticItem {
                category: "Hardware".into(),
                name: "DDC/CI I2C Bus & Group Permissions".into(),
                status: DiagnosticStatus::Warn,
                message: format!("I2C DDC/CI issues detected: {}", issues.join("; ")),
                repair_command: Some(repair.into()),
                unit_identifier: None,
                fix_applied: false,
            }
        } else {
            DiagnosticItem {
                category: "Hardware".into(),
                name: "DDC/CI I2C Bus & Group Permissions".into(),
                status: DiagnosticStatus::Pass,
                message: format!(
                    "Accessible read/write access, i2c group membership, and udev rules confirmed for {} I2C devices",
                    i2c_devs.len()
                ),
                repair_command: None,
                unit_identifier: None,
                fix_applied: false,
            }
        }
    }

    pub fn check_xdg_user_dirs(&self, auto_fix: bool) -> DiagnosticItem {
        let screenshots = expand_home_path(PathBuf::from("~/Pictures/Screenshots"));
        let wallpapers = crate::config::ShellConfig::load(Self::default_config_path())
            .map(|config| expand_home_path(config.desktop.wallpaper_dir))
            .unwrap_or_else(|_| expand_home_path(PathBuf::from("~/Pictures/Wallpapers")));

        if auto_fix {
            let _ = std::fs::create_dir_all(&screenshots);
            let _ = std::fs::create_dir_all(&wallpapers);
        }

        if screenshots.exists() && wallpapers.exists() {
            DiagnosticItem {
                category: "XDG Paths".into(),
                name: "User Media Directories".into(),
                status: DiagnosticStatus::Pass,
                message: "Screenshots and Wallpapers directories ready".into(),
                repair_command: None,
                unit_identifier: None,
                fix_applied: auto_fix,
            }
        } else {
            DiagnosticItem {
                category: "XDG Paths".into(),
                name: "User Media Directories".into(),
                status: DiagnosticStatus::Warn,
                message: "Pictures/Screenshots or Pictures/Wallpapers directory missing".into(),
                repair_command: Some(
                    "mkdir -p ~/Pictures/Screenshots ~/Pictures/Wallpapers".into(),
                ),
                unit_identifier: None,
                fix_applied: false,
            }
        }
    }

    pub fn check_capture(&self) -> DiagnosticItem {
        let has_tesseract = std::process::Command::new("tesseract")
            .arg("--version")
            .output()
            .is_ok();
        let available = shilpo_services::capture::create_backend().is_ok();

        if available {
            DiagnosticItem {
                category: "Media Capture".into(),
                name: "Screen Capture Suite".into(),
                status: DiagnosticStatus::Pass,
                message: format!(
                    "Wayland screencopy screenshot backend is operational{}",
                    if has_tesseract { " (with OCR)" } else { "" }
                ),
                repair_command: None,
                unit_identifier: None,
                fix_applied: false,
            }
        } else {
            DiagnosticItem {
                category: "Media Capture".into(),
                name: "Screen Capture Suite".into(),
                status: DiagnosticStatus::Warn,
                message: "Screen capture backend unavailable".into(),
                repair_command: None,
                unit_identifier: None,
                fix_applied: false,
            }
        }
    }

    pub fn check_polkit_agent(&self) -> DiagnosticItem {
        let helper_path = shilpo_services::polkit::probe_system_helper_path();
        if helper_path.is_none() {
            return DiagnosticItem {
                category: "Authentication".into(),
                name: "Polkit Agent & Helper".into(),
                status: DiagnosticStatus::Fail,
                message: "polkit-agent-helper-1 not found in standard system locations (/usr/lib/polkit-1, /usr/libexec, /usr/lib)".into(),
                repair_command: Some(
                    "sudo apt install polkitd || sudo pacman -S polkit || sudo dnf install polkit".into(),
                ),
                unit_identifier: None,
                fix_applied: false,
            };
        }
        let helper_display = helper_path.as_ref().unwrap().display().to_string();

        // The helper binary check above is static; whether *our* agent is
        // actually registered can only be answered by the running shell
        // daemon, queried the same way `shilpo status` does.
        match crate::cli::adapters::ipc::IpcAdapter::new().telemetry() {
            Ok(telemetry) if telemetry.polkit_service_available => DiagnosticItem {
                category: "Authentication".into(),
                name: "Polkit Agent & Helper".into(),
                status: DiagnosticStatus::Pass,
                message: format!("Polkit agent registered and ready (helper: {helper_display})"),
                repair_command: None,
                unit_identifier: None,
                fix_applied: false,
            },
            Ok(telemetry) => DiagnosticItem {
                category: "Authentication".into(),
                name: "Polkit Agent & Helper".into(),
                status: DiagnosticStatus::Warn,
                message: format!(
                    "Polkit agent state: {}{} (helper: {helper_display})",
                    if telemetry.polkit_state.is_empty() {
                        "unavailable"
                    } else {
                        telemetry.polkit_state.as_str()
                    },
                    if telemetry.polkit_last_error.is_empty() {
                        String::new()
                    } else {
                        format!(" — {}", telemetry.polkit_last_error)
                    }
                ),
                repair_command: None,
                unit_identifier: None,
                fix_applied: false,
            },
            Err(_) => DiagnosticItem {
                category: "Authentication".into(),
                name: "Polkit Agent & Helper".into(),
                status: DiagnosticStatus::Warn,
                message: format!(
                    "Polkit helper binary found at {helper_display}, but the running shilpo daemon could not be reached to confirm agent registration"
                ),
                repair_command: None,
                unit_identifier: None,
                fix_applied: false,
            },
        }
    }

    pub fn check_idle_management(&self) -> DiagnosticItem {
        match crate::cli::adapters::ipc::IpcAdapter::new().telemetry() {
            Ok(telemetry) if telemetry.idle_service_available => {
                let status = if !telemetry.idle_unsupported_actions.is_empty() {
                    DiagnosticStatus::Warn
                } else {
                    DiagnosticStatus::Pass
                };
                let unsupported_note = if !telemetry.idle_unsupported_actions.is_empty() {
                    format!(
                        " (unsupported actions configured: {})",
                        telemetry.idle_unsupported_actions.join(", ")
                    )
                } else {
                    String::new()
                };
                DiagnosticItem {
                    category: "Power & Idle".into(),
                    name: "Idle Management Service".into(),
                    status,
                    message: format!(
                        "ext-idle-notify-v1 ready, {} behaviors registered, {} active inhibits{}",
                        telemetry.idle_registered_behaviors,
                        telemetry.idle_inhibit_count,
                        unsupported_note
                    ),
                    repair_command: None,
                    unit_identifier: None,
                    fix_applied: false,
                }
            }
            Ok(telemetry) => {
                let state_str = if telemetry.idle_state.is_empty() {
                    "unavailable"
                } else {
                    telemetry.idle_state.as_str()
                };
                let err_str = if telemetry.idle_last_error.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", telemetry.idle_last_error)
                };
                DiagnosticItem {
                    category: "Power & Idle".into(),
                    name: "Idle Management Service".into(),
                    status: DiagnosticStatus::Warn,
                    message: format!("Idle management service state: {state_str}{err_str}"),
                    repair_command: None,
                    unit_identifier: None,
                    fix_applied: false,
                }
            }
            Err(_) => DiagnosticItem {
                category: "Power & Idle".into(),
                name: "Idle Management Service".into(),
                status: DiagnosticStatus::Warn,
                message: "Shell daemon is not running; idle status cannot be verified".into(),
                repair_command: Some("shilpo daemon".into()),
                unit_identifier: None,
                fix_applied: false,
            },
        }
    }

    pub fn check_lock_screen(&self) -> DiagnosticItem {
        let config_path = crate::config::default_config_path();
        let pam_service = crate::config::ShellConfig::load_or_create(&config_path)
            .map(|c| c.lock.pam_service)
            .unwrap_or_else(|_| "login".to_string());

        let pam_service_path = std::path::PathBuf::from("/etc/pam.d").join(&pam_service);
        if !pam_service_path.exists() {
            return DiagnosticItem {
                category: "Authentication".into(),
                name: "Lock Screen".into(),
                status: DiagnosticStatus::Fail,
                message: format!(
                    "configured PAM service '{pam_service}' has no /etc/pam.d/{pam_service} file; \
                     the lock screen will be unable to authenticate anyone"
                ),
                repair_command: None,
                unit_identifier: None,
                fix_applied: false,
            };
        }

        let protocol_available =
            shilpo_services::lock_supervisor::probe_session_lock_protocol_available();
        let protocol_note = if protocol_available {
            ""
        } else {
            " (ext-session-lock-v1 not advertised by the compositor or no Wayland session found)"
        };

        match crate::cli::adapters::ipc::IpcAdapter::new().telemetry() {
            Ok(telemetry) => {
                let status = if !protocol_available {
                    DiagnosticStatus::Fail
                } else if telemetry.lock_last_error.is_empty() {
                    DiagnosticStatus::Pass
                } else {
                    DiagnosticStatus::Warn
                };
                let active_note = if telemetry.lock_session_active {
                    " (a lock is currently active)"
                } else {
                    ""
                };
                let err_note = if telemetry.lock_last_error.is_empty() {
                    String::new()
                } else {
                    format!(" — last error: {}", telemetry.lock_last_error)
                };
                DiagnosticItem {
                    category: "Authentication".into(),
                    name: "Lock Screen".into(),
                    status,
                    message: format!(
                        "PAM service '{pam_service}' present{active_note}{err_note}{protocol_note}"
                    ),
                    repair_command: None,
                    unit_identifier: None,
                    fix_applied: false,
                }
            }
            Err(_) => DiagnosticItem {
                category: "Authentication".into(),
                name: "Lock Screen".into(),
                status: DiagnosticStatus::Warn,
                message: "Shell daemon is not running; lock trigger status cannot be verified"
                    .into(),
                repair_command: Some("shilpo daemon".into()),
                unit_identifier: None,
                fix_applied: false,
            },
        }
    }

    pub fn format_report(&self, items: &[DiagnosticItem]) -> String {
        let mut out = String::from("\n=== Shilpo Doctor Diagnostics ===\n\n");
        for item in items {
            let badge = match item.status {
                DiagnosticStatus::Pass => "[✓ PASS]",
                DiagnosticStatus::Warn => "[⚠ WARN]",
                DiagnosticStatus::Fail => "[✗ FAIL]",
            };
            let fix_note = if item.fix_applied {
                " (auto-fixed)"
            } else {
                ""
            };
            let repair_note = item
                .repair_command
                .as_ref()
                .map(|cmd| format!("\n    Repair: {cmd}"))
                .unwrap_or_default();
            let unit_note = item
                .unit_identifier
                .as_ref()
                .map(|u| format!(" [{u}]"))
                .unwrap_or_default();

            out.push_str(&format!(
                "{:10} {:24} {}{} {}{}{}\n",
                badge,
                format!("[{}]", item.category),
                item.name,
                unit_note,
                item.message,
                fix_note,
                repair_note
            ));
        }
        out
    }
}

fn is_command_available(cmd: &str) -> bool {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let p = dir.join(cmd);
            if p.is_file() {
                return true;
            }
        }
    }
    false
}

fn is_bluez_dbus_active() -> bool {
    zbus::blocking::Connection::system()
        .ok()
        .and_then(|conn| {
            conn.call_method(
                Some("org.bluez"),
                "/",
                Some("org.freedesktop.DBus.Peer"),
                "Ping",
                &(),
            )
            .ok()
        })
        .is_some()
}

fn expand_home_path(path: PathBuf) -> PathBuf {
    if let Some(rest) = path.to_str().and_then(|path| path.strip_prefix("~/")) {
        std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(rest)
    } else {
        path
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::expand_home_path;
    use crate::cli::adapters::doctor::{DiagnosticStatus, DoctorChecker};

    #[test]
    fn wallpaper_path_expands_home_prefix() {
        let expanded = expand_home_path(PathBuf::from("~/Pictures/Wallpapers"));
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        assert_eq!(expanded, home.join("Pictures/Wallpapers"));
    }

    #[test]
    fn absolute_wallpaper_path_is_preserved() {
        let path = PathBuf::from("/srv/wallpapers");
        assert_eq!(expand_home_path(path.clone()), path);
    }

    fn write_config(dir: &TempDir, content: &str) -> PathBuf {
        let path = dir.path().join("config.toml");
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn clean_valid_config_passes() {
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "version = 1\n[bar]\nheight = 48\n");
        let item = DoctorChecker::new().check_config_file_at(&path, false);
        assert_eq!(item.status, DiagnosticStatus::Pass);
        assert!(!item.fix_applied);
    }

    #[test]
    fn valid_config_with_unknown_keys_warns_with_details() {
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "version = 1\nbaer = 1\n[bar]\nheight = 48\nheigth = 64\n",
        );
        let item = DoctorChecker::new().check_config_file_at(&path, false);
        assert_eq!(item.status, DiagnosticStatus::Warn);
        assert!(item.message.contains("unknown config key 'baer'"));
        assert!(item.message.contains("suggestion: 'bar'"));
        assert!(item.message.contains("bar.heigth"));
        assert!(item.message.contains("suggestion: 'bar.height'"));
        assert!(item.message.contains(path.display().to_string().as_str()));
    }

    #[test]
    fn invalid_config_fails() {
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "invalid = [ \n");
        let item = DoctorChecker::new().check_config_file_at(&path, false);
        assert_eq!(item.status, DiagnosticStatus::Fail);
    }

    #[test]
    fn auto_fix_never_rewrites_existing_file_with_unknown_keys() {
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "version = 1\nbaer = 1\n");
        let original = std::fs::read_to_string(&path).unwrap();
        let item = DoctorChecker::new().check_config_file_at(&path, true);
        assert_eq!(item.status, DiagnosticStatus::Warn);
        assert!(!item.fix_applied);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn missing_file_warns_and_auto_fix_creates_default() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        let item = DoctorChecker::new().check_config_file_at(&path, false);
        assert_eq!(item.status, DiagnosticStatus::Warn);
        assert!(!path.exists());

        let item = DoctorChecker::new().check_config_file_at(&path, true);
        assert_eq!(item.status, DiagnosticStatus::Pass);
        assert!(item.fix_applied);
        assert!(path.exists());
    }

    #[test]
    fn diagnostics_do_not_assume_extensions_are_bundled() {
        let items = DoctorChecker::new().run_diagnostics(false);

        assert!(
            items
                .iter()
                .all(|item| item.name != "Bundled Weather WASM Extension"),
            "the binary-only installer must not diagnose an optional extension as bundled"
        );
    }
}
