//! The desktop product's subsystems remain namespaced at the crate root.
//!
//! Public callers use the owning subsystem path:
//!
//! ```
//! use shilpo::cli::adapters;
//! use shilpo::config::ShellConfig;
//! use shilpo::settings::SettingsCategory;
//! use shilpo::shell::ShellRuntime;
//! ```
//!
//! Flattened subsystem exports are intentionally unavailable:
//!
//! ```compile_fail
//! use shilpo::adapters;
//! ```
//! ```compile_fail
//! use shilpo::ShellConfig;
//! ```
//! ```compile_fail
//! use shilpo::SettingsCategory;
//! ```
//! ```compile_fail
//! use shilpo::ShellRuntime;
//! ```
//!
//! Historical whole-crate aliases are intentionally unavailable:
//!
//! ```compile_fail
//! use shilpo::shilpo_cli;
//! ```
//! ```compile_fail
//! use shilpo::shilpo_config;
//! ```
//! ```compile_fail
//! use shilpo::shilpo_settings;
//! ```
//! ```compile_fail
//! use shilpo::shilpo_shell;
//! ```

pub mod assets;
pub mod cli;
pub mod config;
pub mod locale;
pub mod lock;
pub mod settings;
pub mod setup;
pub mod shell;

pub use assets::Assets;
pub use cli::parse_duration;
