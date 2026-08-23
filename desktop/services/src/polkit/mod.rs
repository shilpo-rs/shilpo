pub mod agent;
pub mod helper;
pub mod service;
pub mod state;
pub mod types;

#[cfg(test)]
mod tests;

pub use crate::secret::{zeroize_bytes, zeroize_string};
pub use agent::{
    AuthorityClient, POLKIT_AGENT_OBJECT_PATH, POLKIT_AUTHORITY_DESTINATION,
    POLKIT_AUTHORITY_INTERFACE, POLKIT_AUTHORITY_PATH, PolkitAgentServer, lookup_real_name_by_uid,
    lookup_username_by_uid, parse_polkit_identities,
};
pub use helper::{
    HelperEvent, KNOWN_HELPER_PATHS, MockPolkitHelper, PolkitHelper, PolkitHelperSession,
    SystemPolkitHelper, probe_system_helper_path,
};
pub use service::PolkitService;
pub use state::{DEFAULT_INACTIVITY_TIMEOUT_MS, PolkitDomainState};
pub use types::{
    CommandId, CommandResolver, CommandTicket, PolkitCommand, PolkitCommandOutcome, PolkitIdentity,
    PolkitPort, PolkitPromptState, PolkitRejectionReason, PolkitRequest, PolkitSnapshot,
};
