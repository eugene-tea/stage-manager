//! Whether another process is signed as this app is: it satisfies this process's own
//! designated requirement (Security framework, `SecCodeCheckValidity`).
//!
//! The ctl socket asks it of every client ([`super::ctl::Peer`]), judged by the audit token
//! the kernel keeps for the socket's peer (never by a pid or a path, which a process that
//! exits can hand to another): this very binary satisfies it, and so does the `stage-manager`
//! binary of another copy of the app (the dev copy beside `target/`, a release in
//! `/Applications` and one still in Downloads), which carries the same identifier and the same
//! signing certificate (the Developer ID team, or the local signing identity). An ad-hoc
//! signed running app has a requirement only its own exact code satisfies. This process's
//! requirement is read once ([`own_requirement`]).

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::OnceLock;

use objc2_core_foundation::{CFData, CFDictionary, CFRetained, CFString, CFType};

type OSStatus = i32;

/// `kSecCSDefaultFlags`.
const DEFAULT_FLAGS: u32 = 0;

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecGuestAttributeAudit: Option<&'static CFString>;
    fn SecCodeCopySelf(flags: u32, code: *mut *mut c_void) -> OSStatus;
    fn SecCodeCopyDesignatedRequirement(code: *const c_void, flags: u32, requirement: *mut *mut c_void) -> OSStatus;
    fn SecCodeCopyGuestWithAttributes(
        host: *const c_void,
        attributes: *const c_void,
        flags: u32,
        guest: *mut *mut c_void,
    ) -> OSStatus;
    fn SecCodeCheckValidity(code: *const c_void, flags: u32, requirement: *const c_void) -> OSStatus;
}

/// A Security object the call returned with +1, released when dropped; `None` on an error.
fn owned(status: OSStatus, ptr: *mut c_void) -> Option<CFRetained<CFType>> {
    if status != 0 {
        return None;
    }
    // SAFETY: on success the Copy function returned an owned (+1) CF object or null.
    NonNull::new(ptr.cast::<CFType>()).map(|p| unsafe { CFRetained::from_raw(p) })
}

fn raw(object: &CFType) -> *const c_void {
    (object as *const CFType).cast()
}

/// This process's designated requirement (a `SecRequirement`).
struct Requirement(CFRetained<CFType>);

// SAFETY: a SecRequirement is an immutable CF object; the Security framework's code checks
// take it from any thread.
unsafe impl Send for Requirement {}
// SAFETY: as above; it is only ever read.
unsafe impl Sync for Requirement {}

/// This process's designated requirement, read once (`None`: unreadable, e.g. unsigned code;
/// every peer then fails the check).
fn own_requirement() -> Option<&'static CFType> {
    static OWN: OnceLock<Option<Requirement>> = OnceLock::new();
    OWN.get_or_init(|| {
        let mut me = std::ptr::null_mut();
        // SAFETY: a valid out pointer.
        let me = owned(unsafe { SecCodeCopySelf(DEFAULT_FLAGS, &mut me) }, me)?;
        let mut requirement = std::ptr::null_mut();
        // SAFETY: `me` is a live SecCode (a SecStaticCode for this call); a valid out pointer.
        let status = unsafe { SecCodeCopyDesignatedRequirement(raw(&me), DEFAULT_FLAGS, &mut requirement) };
        owned(status, requirement).map(Requirement)
    })
    .as_ref()
    .map(|r| &*r.0)
}

/// Whether this process's own designated requirement can be read (else no peer passes
/// [`signed_like_us`]).
pub fn own_requirement_known() -> bool {
    own_requirement().is_some()
}

/// True when the process with this audit token (the kernel's record of a socket peer,
/// `LOCAL_PEERTOKEN`) is valid code that satisfies this process's designated requirement.
/// False on any error (unsigned app, the peer gone): the caller then trusts it less.
pub fn signed_like_us(audit_token: &[u8]) -> bool {
    let Some(requirement) = own_requirement() else { return false };
    // SAFETY: the Security framework's constant, valid for the life of the process.
    let Some(key) = (unsafe { kSecGuestAttributeAudit }) else { return false };
    let token = CFData::from_bytes(audit_token);
    let attributes = CFDictionary::<CFString, CFData>::from_slices(&[key], &[&token]);
    let attributes_ptr: *const CFDictionary<CFString, CFData> = &*attributes;
    let mut guest = std::ptr::null_mut();
    // SAFETY: a null host is the kernel; `attributes` lives across the call; valid out pointer.
    let status =
        unsafe { SecCodeCopyGuestWithAttributes(std::ptr::null(), attributes_ptr.cast(), DEFAULT_FLAGS, &mut guest) };
    let Some(guest) = owned(status, guest) else { return false };
    // SAFETY: both are live Security objects of the right kinds.
    unsafe { SecCodeCheckValidity(raw(&guest), DEFAULT_FLAGS, raw(requirement)) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::UnixStream;

    #[test]
    fn this_process_is_signed_like_itself_and_garbage_is_not() {
        let (a, _b) = UnixStream::pair().expect("pair");
        let mut token = [0u8; 32];
        let mut len = token.len() as libc::socklen_t;
        // SAFETY: a valid socket; `token` and `len` are valid for the option's size.
        let read = unsafe {
            libc::getsockopt(a.as_raw_fd(), libc::SOL_LOCAL, libc::LOCAL_PEERTOKEN, token.as_mut_ptr().cast(), &mut len)
        };
        assert_eq!(read, 0);
        assert!(own_requirement_known());
        assert!(signed_like_us(&token), "the peer of a socket pair is this test binary");
        assert!(signed_like_us(&token), "again, on the cached requirement");
        assert!(!signed_like_us(&[0u8; 32]), "no such process");
        assert!(!signed_like_us(&[1u8; 5]), "not a token");
    }
}
