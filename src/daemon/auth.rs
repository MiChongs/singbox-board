//! Authorization of control-socket peers via `SO_PEERCRED`.

use std::collections::HashSet;
use std::ffi::CString;

use nix::unistd::{Gid, Group, Uid, User, getgrouplist};

use crate::i18n::fl_log;

#[derive(Debug, Clone)]
pub struct Authorizer {
    daemon_uid: u32,
    allowed_uids: HashSet<u32>,
    group: Option<Gid>,
}

impl Authorizer {
    pub fn new(group_name: Option<&str>, allowed_uids: &[u32]) -> Self {
        let group = group_name.and_then(|name| match Group::from_name(name) {
            Ok(Some(group)) => Some(group.gid),
            Ok(None) => {
                tracing::warn!("{}", fl_log!("auth-group-missing", group = name));
                None
            }
            Err(err) => {
                tracing::warn!(
                    "{}",
                    fl_log!(
                        "auth-group-lookup-failed",
                        group = name,
                        error = err.to_string()
                    )
                );
                None
            }
        });
        Self {
            daemon_uid: Uid::effective().as_raw(),
            allowed_uids: allowed_uids.iter().copied().collect(),
            group,
        }
    }

    pub fn group(&self) -> Option<Gid> {
        self.group
    }

    pub fn is_allowed(&self, uid: u32, gid: u32) -> bool {
        if uid == 0 || uid == self.daemon_uid || self.allowed_uids.contains(&uid) {
            return true;
        }
        let Some(group) = self.group else {
            return false;
        };
        gid == group.as_raw() || user_in_group(uid, group)
    }
}

/// Checks supplementary group membership as recorded in the group database.
fn user_in_group(uid: u32, group: Gid) -> bool {
    let Ok(Some(user)) = User::from_uid(Uid::from_raw(uid)) else {
        return false;
    };
    let Ok(name) = CString::new(user.name) else {
        return false;
    };
    getgrouplist(&name, user.gid)
        .map(|groups| groups.contains(&group))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_and_self_are_allowed() {
        let auth = Authorizer::new(None, &[4242]);
        assert!(auth.is_allowed(0, 0));
        assert!(auth.is_allowed(Uid::effective().as_raw(), 12345));
        assert!(auth.is_allowed(4242, 4242));
        assert!(!auth.is_allowed(65_000, 65_000) || Uid::effective().as_raw() == 65_000);
    }
}
