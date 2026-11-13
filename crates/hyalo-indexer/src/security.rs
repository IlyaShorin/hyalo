//! Who may talk to the service: the system, and the users who installed
//! it for themselves, each by SID. Every name on the volume goes through
//! the pipe, so nobody else may open it, and not even they may make an
//! instance of it, which would let them pose as the service.

use std::io;

use windows::Win32::Foundation::{HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{HSTRING, PWSTR};

use crate::overlapped::{Owned, os};

/// What a user may do with the pipe: read, and write what it asks
/// (`FILE_GENERIC_READ | FILE_WRITE_DATA`), never append, which on a pipe
/// is making an instance of it.
pub const CLIENT_ACCESS: u32 = 0x0012_008B;

/// Whether `text` is a SID as Windows writes one: `S-1-`, then numbers
/// joined by dashes.
pub fn is_sid(text: &str) -> bool {
    let number = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    text.strip_prefix("S-1-")
        .is_some_and(|rest| rest.split('-').all(number))
}

/// The pipe's security, in SDDL: the system may do anything, each of
/// `users` that is a SID what `CLIENT_ACCESS` lets it, nobody else
/// anything; nothing inherited.
pub fn pipe_sddl(users: &[String]) -> String {
    let mut sddl = String::from("D:P(A;;GA;;;SY)");
    for user in users.iter().filter(|user| is_sid(user)) {
        sddl.push_str(&format!("(A;;0x{CLIENT_ACCESS:x};;;{user})"));
    }
    sddl
}

/// The SID of the user this process runs for.
pub fn current_user() -> io::Result<String> {
    let mut token = HANDLE::default();
    // SAFETY: a plain call; the token is closed by `Owned`.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).map_err(os)? };
    let token = Owned(token);
    let mut room = vec![0_u64; 64];
    let mut length = 0;
    // SAFETY: the room is ours and as long as said.
    unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(room.as_mut_ptr().cast()),
            (room.len() * 8) as u32,
            &mut length,
        )
        .map_err(os)?;
    }
    // SAFETY: Windows wrote a TOKEN_USER at the room's start, aligned to 8.
    let user = unsafe { room.as_ptr().cast::<TOKEN_USER>().read() };
    let mut text = PWSTR::null();
    // SAFETY: the SID points into the room, which outlives the call; the
    // text it gives is read once, then freed once.
    unsafe {
        ConvertSidToStringSidW(user.User.Sid, &mut text).map_err(os)?;
        let sid = text.to_string();
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
        sid.map_err(|_| io::ErrorKind::InvalidData.into())
    }
}

/// A security descriptor made from SDDL, freed when dropped.
pub(crate) struct Security(pub(crate) PSECURITY_DESCRIPTOR);

impl Security {
    pub(crate) fn new(sddl: &str) -> io::Result<Security> {
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: a plain call; the descriptor is freed by `Drop`.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &HSTRING::from(sddl),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
            .map_err(os)?;
        }
        Ok(Security(descriptor))
    }
}

impl Drop for Security {
    fn drop(&mut self) {
        // SAFETY: Windows allocated it for us; freed once.
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0.0)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sid_is_s_1_then_numbers_joined_by_dashes() {
        for sid in ["S-1-5-18", "S-1-5-21-1004336348-1177238915-682003330-1001"] {
            assert!(is_sid(sid), "{sid}");
        }
        for not in [
            "",
            "S-1",
            "S-1-",
            "S-1-5-",
            "S-1--5",
            "s-1-5-18",
            "S-2-5-18",
            "S-1-5-18)(A;;GA;;;WD",
            "S-1-5-1x",
            "SY",
        ] {
            assert!(!is_sid(not), "{not}");
        }
    }

    #[test]
    fn the_pipe_lets_the_system_do_anything_and_each_user_read_and_ask() {
        let users = [
            "S-1-5-21-1-2-3-1001".to_string(),
            "S-1-5-21-1-2-3-1002".to_string(),
        ];
        assert_eq!(
            pipe_sddl(&users),
            "D:P(A;;GA;;;SY)(A;;0x12008b;;;S-1-5-21-1-2-3-1001)(A;;0x12008b;;;S-1-5-21-1-2-3-1002)"
        );
    }

    #[test]
    fn what_is_no_sid_gets_nothing() {
        let users = [
            "S-1-5-21-1-2-3-1001)(A;;GA;;;WD".to_string(),
            "WD".to_string(),
            "S-1-5-21-1-2-3-1003".to_string(),
        ];
        assert_eq!(
            pipe_sddl(&users),
            "D:P(A;;GA;;;SY)(A;;0x12008b;;;S-1-5-21-1-2-3-1003)"
        );
        assert_eq!(pipe_sddl(&[]), "D:P(A;;GA;;;SY)", "the system alone");
    }

    #[test]
    fn this_process_runs_for_a_user_with_a_sid() {
        assert!(is_sid(&current_user().expect("a user")));
    }
}
