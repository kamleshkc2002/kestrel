//! The wait loop shared by the Wayland window-list workers.
//!
//! A worker stays blocked on the compositor connection and a wake descriptor
//! until the compositor sends an event or a request arrives.

use std::{
    io,
    os::fd::{AsRawFd, OwnedFd},
    sync::{Arc, mpsc},
};

use wayland_client::{EventQueue, backend::WaylandError};

use super::WindowError;
use crate::wake;

/// Sends worker requests and releases the worker's wait afterwards.
pub(super) struct RequestSender<Request> {
    requests: mpsc::Sender<Request>,
    wake: Arc<OwnedFd>,
}

impl<Request> RequestSender<Request> {
    /// Creates the request channel, its receiver, and the descriptor the worker
    /// waits on.
    pub(super) fn channel() -> io::Result<(Self, mpsc::Receiver<Request>, Arc<OwnedFd>)> {
        let (requests, receiver) = mpsc::channel();
        let wake = wake::event_fd()?;
        Ok((
            Self {
                requests,
                wake: Arc::clone(&wake),
            },
            receiver,
            wake,
        ))
    }

    /// Queues a request and wakes the worker.
    pub(super) fn send(&self, request: Request) -> Result<(), mpsc::SendError<Request>> {
        self.requests.send(request)?;
        wake::signal_event_fd(self.wake.as_raw_fd());
        Ok(())
    }
}

impl<Request> Drop for RequestSender<Request> {
    fn drop(&mut self) {
        // The worker sees the closed channel once it wakes.
        wake::signal_event_fd(self.wake.as_raw_fd());
    }
}

/// Waits for compositor events and requests until the channel closes or the
/// handler returns `false`.
pub(super) fn run<State, Request>(
    queue: &mut EventQueue<State>,
    state: &mut State,
    receiver: &mpsc::Receiver<Request>,
    wake_fd: &OwnedFd,
    mut handle: impl FnMut(&mut EventQueue<State>, &mut State, Request) -> bool,
) -> Result<(), WindowError> {
    loop {
        queue.flush().map_err(connection_error)?;
        queue.dispatch_pending(state).map_err(connection_error)?;
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let mut descriptors = [
            libc::pollfd {
                fd: guard.connection_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake_fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        wake::wait(&mut descriptors).map_err(|error| {
            WindowError::disconnected(format!("the Wayland wait failed: {error}"))
        })?;
        let wayland_ready = descriptors[0].revents & libc::POLLIN != 0;
        if wayland_ready {
            match guard.read() {
                Ok(_) => {}
                Err(WaylandError::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(connection_error(error)),
            }
            queue.dispatch_pending(state).map_err(connection_error)?;
        } else {
            drop(guard);
        }
        if descriptors[0].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(WindowError::disconnected("the Wayland connection closed"));
        }
        if descriptors[1].revents == 0 {
            continue;
        }
        wake::drain_event_fd(wake_fd.as_raw_fd());
        loop {
            match receiver.try_recv() {
                Ok(request) => {
                    if !handle(queue, state, request) {
                        return Ok(());
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
            }
        }
    }
}

fn connection_error(error: impl std::fmt::Display) -> WindowError {
    WindowError::disconnected(format!("the Wayland connection failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::net::UnixStream, thread, time::Duration};

    /// How long a test waits for the worker to answer a request or stop.
    const TEST_TIMEOUT: Duration = Duration::from_millis(100);

    enum TestRequest {
        Snapshot(mpsc::Sender<()>),
        Stop,
    }

    fn spawn_worker() -> (
        RequestSender<TestRequest>,
        mpsc::Receiver<Result<(), WindowError>>,
        thread::JoinHandle<()>,
    ) {
        let (client, server) = UnixStream::pair().expect("socket pair");
        let connection = wayland_client::Connection::from_socket(client).expect("connection");
        let (sender, receiver, wake) = RequestSender::channel().expect("wake descriptor");
        let (done_sender, done_receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            // The peer stays open and silent, so only the wake descriptor makes
            // the loop return.
            let _server = server;
            let mut queue = connection.new_event_queue();
            let result = run(
                &mut queue,
                &mut (),
                &receiver,
                &wake,
                |_, _, request| match request {
                    TestRequest::Snapshot(reply) => {
                        let _ = reply.send(());
                        true
                    }
                    TestRequest::Stop => false,
                },
            );
            let _ = done_sender.send(result);
        });
        (sender, done_receiver, worker)
    }

    #[test]
    fn a_request_wakes_the_worker_without_compositor_traffic() {
        let (sender, done, worker) = spawn_worker();
        let (reply, reply_receiver) = mpsc::channel();
        sender
            .send(TestRequest::Snapshot(reply))
            .expect("request is queued");
        reply_receiver
            .recv_timeout(TEST_TIMEOUT)
            .expect("the worker answers a request while the connection is silent");
        sender.send(TestRequest::Stop).expect("stop is queued");
        assert!(
            done.recv_timeout(TEST_TIMEOUT)
                .expect("the worker stops")
                .is_ok()
        );
        worker.join().expect("the worker joins");
    }

    #[test]
    fn a_dropped_sender_ends_the_worker() {
        let (sender, done, worker) = spawn_worker();
        drop(sender);
        assert!(
            done.recv_timeout(TEST_TIMEOUT)
                .expect("the worker stops")
                .is_ok()
        );
        worker.join().expect("the worker joins");
    }
}
