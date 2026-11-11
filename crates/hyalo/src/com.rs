//! COM on the current thread, for as long as the guard lives.

use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, COINIT_MULTITHREADED, CoInitializeEx,
    CoUninitialize,
};
use windows::core::Result;

/// Proof that COM is initialized on this thread. Not `Send`: COM belongs
/// to the thread that joined it.
pub struct Com(std::marker::PhantomData<*const ()>);

impl Com {
    /// Joins a single-threaded apartment, which shell objects expect.
    pub fn init() -> Result<Self> {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).ok()? };
        Ok(Self(std::marker::PhantomData))
    }

    /// Joins the multithreaded apartment, where waiting for a Windows
    /// Runtime operation to finish holds up no window.
    pub fn multithreaded() -> Result<Self> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
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
