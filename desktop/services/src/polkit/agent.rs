use std::collections::HashMap;
use std::ffi::CStr;
use std::future::Future;
use std::mem::MaybeUninit;
use std::sync::Arc;

use zbus::fdo::DBusProxy;
use zbus::message::Header;
use zbus::zvariant::{self, OwnedValue, Value};
use zbus::{Connection, interface};

use super::state::PolkitDomainState;
use super::types::{PolkitIdentity, PolkitRequest};

pub const POLKIT_AGENT_OBJECT_PATH: &str = "/org/shilpo/PolicyKit1/AuthenticationAgent";
pub const POLKIT_AUTHORITY_DESTINATION: &str = "org.freedesktop.PolicyKit1";
pub const POLKIT_AUTHORITY_PATH: &str = "/org/freedesktop/PolicyKit1/Authority";
pub const POLKIT_AUTHORITY_INTERFACE: &str = "org.freedesktop.PolicyKit1.Authority";

/// Resolves a Unix UID to its username using `getpwuid_r`.
pub fn lookup_username_by_uid(uid: u32) -> Option<String> {
    let mut pwd = MaybeUninit::<libc::passwd>::uninit();
    let mut pwd_ptr = std::ptr::null_mut();
    let mut buf = vec![0u8; 1024];
    loop {
        let res = unsafe {
            libc::getpwuid_r(
                uid as libc::uid_t,
                pwd.as_mut_ptr(),
                buf.as_mut_ptr() as *mut libc::c_char,
                buf.len(),
                &mut pwd_ptr,
            )
        };
        if res == 0 {
            if pwd_ptr.is_null() {
                return None;
            }
            let pwd_struct = unsafe { pwd.assume_init() };
            if pwd_struct.pw_name.is_null() {
                return None;
            }
            let name_cstr = unsafe { CStr::from_ptr(pwd_struct.pw_name) };
            return Some(name_cstr.to_string_lossy().into_owned());
        } else if res == libc::ERANGE {
            buf.resize(buf.len() * 2, 0);
        } else {
            return None;
        }
    }
}

/// Resolves a Unix UID to the user's real / GECOS name.
pub fn lookup_real_name_by_uid(uid: u32) -> Option<String> {
    let mut pwd = MaybeUninit::<libc::passwd>::uninit();
    let mut pwd_ptr = std::ptr::null_mut();
    let mut buf = vec![0u8; 1024];
    loop {
        let res = unsafe {
            libc::getpwuid_r(
                uid as libc::uid_t,
                pwd.as_mut_ptr(),
                buf.as_mut_ptr() as *mut libc::c_char,
                buf.len(),
                &mut pwd_ptr,
            )
        };
        if res == 0 {
            if pwd_ptr.is_null() {
                return None;
            }
            let pwd_struct = unsafe { pwd.assume_init() };
            if !pwd_struct.pw_gecos.is_null() {
                let gecos_cstr = unsafe { CStr::from_ptr(pwd_struct.pw_gecos) };
                let gecos_str = gecos_cstr.to_string_lossy();
                let real_name = gecos_str.split(',').next().unwrap_or("").trim();
                if !real_name.is_empty() {
                    return Some(real_name.to_string());
                }
            }
            return None;
        } else if res == libc::ERANGE {
            buf.resize(buf.len() * 2, 0);
        } else {
            return None;
        }
    }
}

fn extract_uid_from_value(value: &Value) -> Option<u32> {
    match value {
        Value::U32(u) => Some(*u),
        Value::I32(i) if *i >= 0 => Some(*i as u32),
        Value::U64(u) => Some(*u as u32),
        Value::I64(i) if *i >= 0 => Some(*i as u32),
        Value::Str(s) => s.parse().ok(),
        _ => None,
    }
}

/// Parses raw PolicyKit D-Bus identities into typed `PolkitIdentity` structs.
pub fn parse_polkit_identities(
    raw: Vec<(String, HashMap<String, OwnedValue>)>,
) -> Vec<PolkitIdentity> {
    let mut identities = Vec::new();
    for (kind, dict) in raw {
        if kind == "unix-user" {
            let uid = dict.get("uid").and_then(|v| extract_uid_from_value(v));
            if let Some(uid) = uid {
                let user_name = dict
                    .get("name")
                    .and_then(|v| match &**v {
                        Value::Str(s) => Some(s.to_string()),
                        _ => None,
                    })
                    .or_else(|| lookup_username_by_uid(uid))
                    .unwrap_or_else(|| format!("uid-{uid}"));

                let real_name = lookup_real_name_by_uid(uid);
                let mut identity = PolkitIdentity::new(kind, uid, user_name);
                if let Some(rn) = real_name {
                    identity = identity.with_real_name(rn);
                }
                identities.push(identity);
            }
        } else if kind == "unix-group" || kind == "unix-netgroup" {
            // Group identities can also be mapped
            let name = dict
                .get("name")
                .and_then(|v| match &**v {
                    Value::Str(s) => Some(s.to_string()),
                    _ => None,
                })
                .unwrap_or_else(|| kind.clone());
            identities.push(PolkitIdentity::new(kind, 0, name));
        }
    }
    identities
}

/// D-Bus interface server for `org.freedesktop.PolicyKit1.AuthenticationAgent`.
pub struct PolkitAgentServer {
    state: Arc<PolkitDomainState>,
}

impl PolkitAgentServer {
    pub fn new(state: Arc<PolkitDomainState>) -> Self {
        Self { state }
    }
}

/// Authorizes a PolicyKit agent method caller through an injectable UID
/// resolver. Keeping the resolver outside the policy decision makes the
/// security boundary deterministic in tests without a real system bus.
pub(crate) async fn authorize_polkit_caller_with<F, Fut>(
    sender: Option<&str>,
    resolve_uid: F,
) -> zbus::fdo::Result<()>
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Result<u32, String>>,
{
    let sender = sender.ok_or_else(|| {
        zbus::fdo::Error::AccessDenied("PolicyKit caller has no D-Bus sender".into())
    })?;
    let uid = resolve_uid(sender.to_owned()).await.map_err(|_| {
        zbus::fdo::Error::AccessDenied("could not verify PolicyKit caller identity".into())
    })?;
    if uid != 0 {
        return Err(zbus::fdo::Error::AccessDenied(
            "PolicyKit agent methods are restricted to root callers".into(),
        ));
    }
    Ok(())
}

async fn authorize_polkit_caller(
    header: &Header<'_>,
    connection: &Connection,
) -> zbus::fdo::Result<()> {
    authorize_polkit_caller_with(
        header.sender().map(|sender| sender.as_str()),
        |sender| async move {
            let proxy = DBusProxy::new(connection)
                .await
                .map_err(|err| err.to_string())?;
            let bus_name = sender
                .try_into()
                .map_err(|err: zbus::names::Error| err.to_string())?;
            proxy
                .get_connection_unix_user(bus_name)
                .await
                .map_err(|err| err.to_string())
        },
    )
    .await
}

#[interface(name = "org.freedesktop.PolicyKit1.AuthenticationAgent")]
impl PolkitAgentServer {
    /// Authority calls `BeginAuthentication` to request user authentication.
    ///
    /// This method call remains open until authentication succeeds, fails, or is cancelled.
    #[allow(clippy::too_many_arguments)]
    async fn begin_authentication(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
        action_id: String,
        message: String,
        icon_name: String,
        _details: HashMap<String, String>,
        cookie: String,
        identities: Vec<(String, HashMap<String, OwnedValue>)>,
    ) -> zbus::fdo::Result<()> {
        authorize_polkit_caller(&header, connection).await?;

        let typed_identities = parse_polkit_identities(identities);
        let (tx, rx) = tokio::sync::oneshot::channel();

        let request = PolkitRequest {
            action_id,
            message,
            icon_name,
            cookie,
            is_internal: false,
            identities: typed_identities,
            selected_identity: None,
        };

        if let Err(err) = self.state.begin_authentication(request, tx) {
            return Err(zbus::fdo::Error::Failed(err));
        }

        // Await completion from user interaction, helper success/failure, authority cancellation, or timeout
        match rx.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => Err(zbus::fdo::Error::Failed(err)),
            Err(_) => Err(zbus::fdo::Error::Failed(
                "Authentication agent session dropped".into(),
            )),
        }
    }

    /// Authority calls `CancelAuthentication` when an authentication request is cancelled.
    async fn cancel_authentication(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
        cookie: String,
    ) -> zbus::fdo::Result<()> {
        authorize_polkit_caller(&header, connection).await?;
        self.state.cancel_authentication(&cookie);
        Ok(())
    }
}

/// True if a `GetSessionByPID` error indicates logind has no session for this
/// pid specifically (as opposed to logind being unreachable, permission
/// denied, or some other unrelated failure).
fn is_no_session_error(err: &str) -> bool {
    err.contains("No session for pid") || err.contains("NoSuchSession") || err.contains("not found")
}

fn subject_from_session_id(session_id: &str) -> (String, HashMap<String, Value<'static>>) {
    let mut dict = HashMap::new();
    dict.insert(
        "session-id".to_string(),
        Value::from(session_id.to_string()),
    );
    ("unix-session".to_string(), dict)
}

fn subject_from_uid(uid: u32) -> (String, HashMap<String, Value<'static>>) {
    let mut dict = HashMap::new();
    dict.insert("uid".to_string(), Value::from(uid));
    ("unix-user".to_string(), dict)
}

/// Helper functions for registering and unregistering with PolicyKit Authority.
pub struct AuthorityClient;

impl AuthorityClient {
    /// Resolves the subject tuple `(subject_kind, subject_details)` using the 3-tier fallback chain:
    /// 1. `XDG_SESSION_ID` -> `("unix-session", {"session-id": session_id})`
    /// 2. Session for own PID via `logind` -> `("unix-session", {"session-id": session_id})`
    /// 3. If step 2 fails *specifically* because logind has no session for this
    ///    pid, fall back to `("unix-user", {"uid": getuid()})`. Any other error
    ///    (logind unreachable, permission denied, ...) is propagated instead of
    ///    silently substituting a possibly-wrong subject.
    pub async fn resolve_subject(
        system_conn: &zbus::Connection,
    ) -> Result<(String, HashMap<String, Value<'static>>), zbus::Error> {
        // 1. Check XDG_SESSION_ID
        if let Ok(session_id) = std::env::var("XDG_SESSION_ID") {
            let session_id = session_id.trim();
            if !session_id.is_empty() {
                return Ok(subject_from_session_id(session_id));
            }
        }

        // 2. Query logind for our own pid
        let pid = std::process::id();
        let logind_res: Result<zvariant::OwnedObjectPath, zbus::Error> = system_conn
            .call_method(
                Some("org.freedesktop.login1"),
                "/org/freedesktop/login1",
                Some("org.freedesktop.login1.Manager"),
                "GetSessionByPID",
                &(pid,),
            )
            .await
            .and_then(|reply| reply.body().deserialize());

        match logind_res {
            Ok(path) => {
                let path_str = path.as_str();
                match path_str.split('/').next_back() {
                    Some(session_id) if !session_id.is_empty() => {
                        Ok(subject_from_session_id(session_id))
                    }
                    // 3. logind returned an empty/malformed session id: unix-user.
                    _ => Ok(subject_from_uid(unsafe { libc::getuid() })),
                }
            }
            // 3. No session for this pid specifically: fall back to unix-user.
            Err(err) if is_no_session_error(&err.to_string()) => {
                Ok(subject_from_uid(unsafe { libc::getuid() }))
            }
            // Any other error is a genuine failure, not a "no session" condition.
            Err(err) => Err(err),
        }
    }

    /// Registers the authentication agent at `object_path` on the PolicyKit Authority.
    pub async fn register_agent(
        system_conn: &zbus::Connection,
        object_path: &str,
    ) -> Result<(String, HashMap<String, Value<'static>>), zbus::Error> {
        let (subject_kind, subject_dict) = Self::resolve_subject(system_conn).await?;
        let locale = std::env::var("LANG").unwrap_or_else(|_| "C.UTF-8".to_string());

        system_conn
            .call_method(
                Some(POLKIT_AUTHORITY_DESTINATION),
                POLKIT_AUTHORITY_PATH,
                Some(POLKIT_AUTHORITY_INTERFACE),
                "RegisterAuthenticationAgent",
                &((&subject_kind, &subject_dict), &locale, object_path),
            )
            .await?;

        Ok((subject_kind, subject_dict))
    }

    /// Unregisters the authentication agent from PolicyKit Authority on shutdown.
    pub async fn unregister_agent(
        system_conn: &zbus::Connection,
        subject: &(String, HashMap<String, Value<'static>>),
        object_path: &str,
    ) -> Result<(), zbus::Error> {
        system_conn
            .call_method(
                Some(POLKIT_AUTHORITY_DESTINATION),
                POLKIT_AUTHORITY_PATH,
                Some(POLKIT_AUTHORITY_INTERFACE),
                "UnregisterAuthenticationAgent",
                &((&subject.0, &subject.1), object_path),
            )
            .await?;

        Ok(())
    }
}
