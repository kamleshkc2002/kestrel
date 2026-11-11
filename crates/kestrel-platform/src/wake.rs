//! Descriptor wakeups shared by the clipboard change watch and the window workers.

use std::{
    io,
    os::fd::{FromRawFd, OwnedFd, RawFd},
    sync::Arc,
};

/// Creates a non-blocking event descriptor that a wait can block on and another
/// thread can signal.
pub(crate) fn event_fd() -> io::Result<Arc<OwnedFd>> {
    // SAFETY: `eventfd` has no preconditions; the returned descriptor is owned here.
    let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a freshly created descriptor that nothing else owns.
    Ok(Arc::new(unsafe { OwnedFd::from_raw_fd(fd) }))
}

/// Releases a wait that is blocked on the descriptor.
pub(crate) fn signal_event_fd(fd: RawFd) {
    let value: u64 = 1;
    // SAFETY: `fd` is an eventfd and `value` is an eight-byte integer.
    unsafe {
        libc::write(
            fd,
            std::ptr::addr_of!(value).cast(),
            std::mem::size_of::<u64>(),
        );
    }
}

/// Consumes a signal so the descriptor stops reporting readiness.
pub(crate) fn drain_event_fd(fd: RawFd) {
    let mut value: u64 = 0;
    // SAFETY: `fd` is an eventfd and `value` is an eight-byte integer.
    unsafe {
        libc::read(
            fd,
            std::ptr::addr_of_mut!(value).cast(),
            std::mem::size_of::<u64>(),
        );
    }
}

/// Blocks until one of the descriptors is ready, retrying interrupted waits.
pub(crate) fn wait(descriptors: &mut [libc::pollfd]) -> io::Result<()> {
    loop {
        // SAFETY: `descriptors` points at initialised `pollfd` values.
        let ready = unsafe {
            libc::poll(
                descriptors.as_mut_ptr(),
                descriptors.len() as libc::nfds_t,
                -1,
            )
        };
        if ready >= 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}
