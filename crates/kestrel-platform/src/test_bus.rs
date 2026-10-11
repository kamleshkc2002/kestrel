//! A private `dbus-daemon` serving fake desktop objects for tests.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use zbus::blocking::connection::Builder;

type Serve = Box<dyn FnOnce(Builder<'static>) -> zbus::Result<Builder<'static>> + Send>;

pub(crate) struct PrivateBus {
    pub(crate) address: String,
    stop: Arc<AtomicBool>,
    daemon: Child,
    thread: Option<thread::JoinHandle<()>>,
}

impl PrivateBus {
    /// Starts a bus on which a connection owning `name` serves what `serve`
    /// adds; `None` when `dbus-daemon` is missing.
    pub(crate) fn start(
        name: &'static str,
        serve: impl FnOnce(Builder<'static>) -> zbus::Result<Builder<'static>> + Send + 'static,
    ) -> Option<Self> {
        let Ok(mut daemon) = Command::new("dbus-daemon")
            .args(["--session", "--print-address", "--nofork"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            eprintln!("skipping: dbus-daemon is not available");
            return None;
        };
        let mut address = String::new();
        BufReader::new(daemon.stdout.take().expect("piped stdout"))
            .read_line(&mut address)
            .expect("read the private bus address");
        let address = address.trim().to_owned();
        assert!(!address.is_empty(), "dbus-daemon printed an address");
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel();
        let worker_address = address.clone();
        let worker_stop = Arc::clone(&stop);
        let serve: Serve = Box::new(serve);
        let thread = thread::Builder::new()
            .name("kestrel-test-bus".to_owned())
            .spawn(move || {
                let connection = Builder::address(worker_address.as_str())
                    .and_then(|builder| builder.name(name))
                    .and_then(serve)
                    .and_then(|builder| builder.build());
                let connection = match connection {
                    Ok(connection) => connection,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error.to_string()));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(()));
                while !worker_stop.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(5));
                }
                let _ = connection.close();
            })
            .expect("spawn the fake service");
        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("fake service starts")
            .expect("fake service owns its name");
        Some(Self {
            address,
            stop,
            daemon,
            thread: Some(thread),
        })
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}
