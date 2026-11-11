//! COM on the current thread, for as long as the guard lives; and the
//! multithreaded apartment, for as long as the process.

use std::sync::OnceLock;

use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoIncrementMTAUsage, CoInitializeEx,
    CoUninitialize,
};
use windows::core::{HRESULT, Result};

/// Proof that COM is initialized on this thread. Not `Send`: COM belongs
/// to the thread that joined it.
pub struct Com(std::marker::PhantomData<*const ()>);

impl Com {
    /// Joins a single-threaded apartment, which shell objects expect.
    pub fn init() -> Result<Self> {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).ok()? };
        Ok(Self(std::marker::PhantomData))
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        // SAFETY: `init` succeeded on this thread (the guard cannot move to
        // another one), so this balances its CoInitializeEx.
        unsafe { CoUninitialize() };
    }
}

/// Keeps the multithreaded apartment up for the rest of the process, so a
/// thread of no apartment of its own may use the Windows Runtime and wait
/// for it. Joined and left around each wait instead, the apartment could
/// be torn down while the callback that ended the wait was still
/// returning. The first call starts it; later ones say how that went.
pub fn keep_multithreaded() -> Result<()> {
    static KEPT: OnceLock<HRESULT> = OnceLock::new();
    // SAFETY: a plain call; the usage it takes is never given back.
    let kept = KEPT.get_or_init(|| {
        unsafe { CoIncrementMTAUsage() }.map_or_else(|error| error.code(), |_| HRESULT(0))
    });
    kept.ok()
}
