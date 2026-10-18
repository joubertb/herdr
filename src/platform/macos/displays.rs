use std::sync::OnceLock;

type CgDirectDisplayId = u32;
type CgError = i32;
type BooleanT = libc::c_int;

const MAX_DISPLAYS: usize = 32;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGGetOnlineDisplayList(
        max_displays: u32,
        online_displays: *mut CgDirectDisplayId,
        display_count: *mut u32,
    ) -> CgError;
    fn CGDisplayIsActive(display: CgDirectDisplayId) -> BooleanT;
    fn CGDisplayIsAsleep(display: CgDirectDisplayId) -> BooleanT;
}

/// Counts displays that are online, active, and awake.
///
/// Returns `None` when the host terminal is not on this Mac's displays (an SSH
/// session) or the window server cannot be queried.
pub(crate) fn awake_display_count() -> Option<u32> {
    static LOCAL_SESSION: OnceLock<bool> = OnceLock::new();
    let local = *LOCAL_SESSION.get_or_init(|| {
        std::env::var_os("SSH_CONNECTION").is_none() && std::env::var_os("SSH_TTY").is_none()
    });
    if !local {
        return None;
    }

    let mut displays = [0 as CgDirectDisplayId; MAX_DISPLAYS];
    let mut count = 0u32;
    // SAFETY: the buffer holds MAX_DISPLAYS entries and CoreGraphics writes at
    // most that many, reporting how many it wrote through `count`.
    let err =
        unsafe { CGGetOnlineDisplayList(MAX_DISPLAYS as u32, displays.as_mut_ptr(), &mut count) };
    if err != 0 {
        return None;
    }
    let online = displays.get(..count as usize)?;
    let awake = online
        .iter()
        // SAFETY: plain queries on display IDs returned by CoreGraphics; an ID
        // that went away since the list call reads as inactive.
        .filter(|&&display| unsafe {
            CGDisplayIsActive(display) != 0 && CGDisplayIsAsleep(display) == 0
        })
        .count();
    u32::try_from(awake).ok()
}
