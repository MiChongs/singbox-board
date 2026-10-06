//! Authorization of named pipe clients by their token.

use crate::i18n::fl_log;
use crate::win::{Identity, Sid, lookup_account};

/// SYSTEM, elevated administrators, the daemon's own account and
/// members of the socket group may talk to the daemon. The pipe's DACL
/// enforces the same before a client gets a handle.
pub struct Authorizer {
    daemon_user: Option<Sid>,
    group: Option<Sid>,
}

impl Authorizer {
    pub fn new(group_name: Option<&str>, allowed_uids: &[u32]) -> Self {
        if !allowed_uids.is_empty() {
            tracing::warn!("{}", fl_log!("win-auth-uids-ignored"));
        }
        let group = group_name.and_then(|name| match lookup_account(name) {
            Ok(sid) => Some(sid),
            Err(err) => {
                tracing::warn!(
                    "{}",
                    fl_log!(
                        "win-auth-group-missing",
                        group = name,
                        error = err.to_string()
                    )
                );
                None
            }
        });
        Self {
            daemon_user: Identity::current().ok().map(|identity| identity.user),
            group,
        }
    }

    pub fn group(&self) -> Option<&Sid> {
        self.group.as_ref()
    }

    pub fn is_allowed(&self, client: &Identity) -> bool {
        client.admin
            || self.daemon_user.as_ref() == Some(&client.user)
            || self
                .group
                .as_ref()
                .is_some_and(|group| client.member_of(group))
    }

    /// May decide what runs as SYSTEM: an administrator or the daemon's
    /// own account.
    pub fn is_privileged(&self, client: &Identity) -> bool {
        client.admin || self.daemon_user.as_ref() == Some(&client.user)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_daemons_own_account_is_allowed() {
        let auth = Authorizer::new(Some("no-such-group-for-singbox-board"), &[]);
        assert!(auth.group().is_none());
        let me = Identity::current().unwrap();
        assert!(auth.is_allowed(&me));
        assert!(auth.is_privileged(&me));
    }
}
