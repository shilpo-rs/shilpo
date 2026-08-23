pub mod helper;
pub mod pam_child;
pub mod service;
pub mod state;
pub mod types;

#[cfg(test)]
mod tests;

pub use crate::secret::zeroize_string;
pub use helper::{
    AuthHelper, AuthHelperEvent, AuthHelperSession, MockAuthHelper, PAM_HELPER_ENV_VAR,
    SystemAuthHelper,
};
pub use service::AuthService;
pub use state::{AuthDomainState, DEFAULT_INACTIVITY_TIMEOUT_MS, SUCCESS_DISMISS_DELAY_MS};
pub use types::{
    AuthCommand, AuthCommandOutcome, AuthOutcome, AuthPort, AuthPromptState, AuthRejectionReason,
    AuthSnapshot, CommandId, CommandResolver, CommandTicket,
};
