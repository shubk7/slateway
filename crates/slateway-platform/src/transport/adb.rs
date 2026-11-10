// SPDX-License-Identifier: GPL-3.0-or-later

//! adb reverse transport (`docs/PROTOCOL.md`, "Transports"): the
//! development default. Runs `adb reverse` so the tablet dials host
//! loopback over the USB link, then accepts the connection. Needs USB
//! debugging on the tablet (`docs/DEVELOPMENT.md`, "Tablet setup").

use std::{net::SocketAddr, path::PathBuf, process, process::Stdio};

use async_trait::async_trait;
use std::sync::Arc;
use tokio::process::Command;

use super::{BoxStream, DEFAULT_PORT, LoopbackTransport, Transport, TransportError};

/// Executes adb commands. The seam exists for tests: the real runner
/// spawns the adb executable; tests record the arguments and fake the
/// result instead of exec-ing freshly written helper files, which
/// races (`ETXTBSY`) under parallel test load.
#[async_trait]
trait AdbRunner: Send + Sync {
    async fn run(&self, args: &[String], step: &'static str) -> Result<(), TransportError>;
}

/// Spawns the adb executable from `path`.
struct SystemAdb {
    path: PathBuf,
}

#[async_trait]
impl AdbRunner for SystemAdb {
    async fn run(&self, args: &[String], step: &'static str) -> Result<(), TransportError> {
        // adb's stdout is noise (some versions echo the forwarded port
        // on a successful reverse); stderr is captured instead of
        // inherited because the daemon retries the tunnel registration
        // periodically and one self-contained error line beats adb
        // scribbling on the terminal every 2 s.
        let output = Command::new(&self.path)
            .args(args)
            .stdout(Stdio::null())
            .output()
            .await?;
        if !output.status.success() {
            let detail = String::from_utf8_lossy(&output.stderr)
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .map(|line| {
                    let line = line.trim();
                    let mut truncated: String = line.chars().take(120).collect();
                    if truncated.chars().count() < line.chars().count() {
                        truncated.push('…');
                    }
                    truncated
                });
            return Err(TransportError::Adb {
                step,
                code: output.status.code(),
                detail,
            });
        }
        Ok(())
    }
}

/// adb reverse transport: registers the reverse tunnel with adb, then
/// accepts the tablet's connection on host loopback.
pub struct AdbReverseTransport {
    listener: LoopbackTransport,
    runner: Arc<dyn AdbRunner>,
    /// adb executable path, kept for the drop cleanup spawn.
    adb_path: PathBuf,
    port: u16,
}

impl AdbReverseTransport {
    /// Transport on [`DEFAULT_PORT`], using `adb` from `PATH`.
    pub fn new() -> Self {
        Self::with_port(DEFAULT_PORT)
    }

    /// Transport on a fixed port, using `adb` from `PATH`.
    pub fn with_port(port: u16) -> Self {
        Self {
            listener: LoopbackTransport::with_port(port),
            runner: Arc::new(SystemAdb {
                path: PathBuf::from("adb"),
            }),
            adb_path: PathBuf::from("adb"),
            port,
        }
    }

    /// Uses a specific adb executable instead of `adb` from `PATH`.
    pub fn with_adb_path(mut self, adb: impl Into<PathBuf>) -> Self {
        self.adb_path = adb.into();
        self.runner = Arc::new(SystemAdb {
            path: self.adb_path.clone(),
        });
        self
    }

    /// The TCP port the tunnel and the listener use.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Test seam: runs adb commands through `runner` instead of
    /// spawning the adb executable.
    #[cfg(test)]
    fn with_runner(runner: Arc<dyn AdbRunner>, port: u16) -> Self {
        Self {
            listener: LoopbackTransport::with_port(port),
            runner,
            // Tests stay hermetic: the drop cleanup would try to spawn
            // adb; a nonexistent path fails that spawn silently.
            adb_path: PathBuf::from("/nonexistent/slateway-test-adb"),
            port,
        }
    }

    async fn run_adb(&self, args: &[String], step: &'static str) -> Result<(), TransportError> {
        self.runner.run(args, step).await
    }
}

impl Default for AdbReverseTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Transport for AdbReverseTransport {
    async fn prepare(&mut self) -> Result<Option<SocketAddr>, TransportError> {
        // Bind first: when the tablet dials through the tunnel, the
        // listener must already be there.
        let addr = self.listener.prepare().await?;
        let tcp = format!("tcp:{}", self.port);
        self.run_adb(&["reverse".into(), tcp.clone(), tcp], "reverse")
            .await?;
        Ok(addr)
    }

    async fn stream(&mut self) -> Result<BoxStream, TransportError> {
        self.listener.stream().await
    }
}

impl AdbReverseTransport {
    /// Removes the reverse tunnel. The session lifecycle calls this on
    /// teardown; dropping without it leaves the tunnel registered with
    /// the adb server.
    pub async fn cleanup(&mut self) -> Result<(), TransportError> {
        let tcp = format!("tcp:{}", self.port);
        self.run_adb(
            &["reverse".into(), "--remove".into(), tcp],
            "reverse --remove",
        )
        .await
    }
}

impl Drop for AdbReverseTransport {
    fn drop(&mut self) {
        // Best effort, detached: teardown must never block on adb, so
        // the child is spawned and not waited on (a short-lived zombie
        // is acceptable; the explicit `cleanup` is the reliable path).
        // Its output is silenced so a failing remove does not scribble
        // over the terminal after shutdown.
        let tcp = format!("tcp:{}", self.port);
        let _ = process::Command::new(&self.adb_path)
            .args(["reverse", "--remove", &tcp])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    /// Distinct ports per test, from a fixed unprivileged range, so
    /// parallel tests never collide on a listener.
    fn test_port() -> u16 {
        use std::sync::atomic::{AtomicU16, Ordering};
        static NEXT_PORT: AtomicU16 = AtomicU16::new(49000);
        NEXT_PORT.fetch_add(1, Ordering::SeqCst)
    }

    /// Records every adb invocation and fakes the result: no process
    /// is spawned, so tests are hermetic and race-free.
    struct RecordingAdb {
        invocations: Mutex<Vec<Vec<String>>>,
        code: i32,
    }

    impl RecordingAdb {
        fn working() -> Arc<Self> {
            Arc::new(Self {
                invocations: Mutex::new(Vec::new()),
                code: 0,
            })
        }

        fn failing(code: i32) -> Arc<Self> {
            Arc::new(Self {
                invocations: Mutex::new(Vec::new()),
                code,
            })
        }

        fn recorded(&self) -> Vec<Vec<String>> {
            self.invocations.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl AdbRunner for RecordingAdb {
        async fn run(&self, args: &[String], step: &'static str) -> Result<(), TransportError> {
            self.invocations.lock().unwrap().push(args.to_vec());
            if self.code == 0 {
                Ok(())
            } else {
                Err(TransportError::Adb {
                    step,
                    code: Some(self.code),
                    detail: None,
                })
            }
        }
    }

    #[tokio::test]
    async fn registers_tunnel_and_accepts() {
        let adb = RecordingAdb::working();
        let port = test_port();

        let mut transport = AdbReverseTransport::with_runner(adb.clone(), port);
        let addr = transport.prepare().await.unwrap().unwrap();
        assert_eq!(transport.port(), port);
        assert!(addr.ip().is_loopback());
        assert_eq!(addr.port(), port);

        // The tunnel was registered before the tablet dials.
        assert_eq!(
            adb.recorded(),
            vec![vec![
                "reverse".to_string(),
                format!("tcp:{port}"),
                format!("tcp:{port}"),
            ]]
        );

        // The tablet side dials host loopback (adb's forward is the
        // part faked away); the transport accepts it.
        let mut tablet = TcpStream::connect(addr).await.unwrap();
        let mut host = transport.stream().await.unwrap();
        tablet.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        host.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
    }

    #[tokio::test]
    async fn cleanup_removes_the_tunnel() {
        let adb = RecordingAdb::working();
        let port = test_port();
        let mut transport = AdbReverseTransport::with_runner(adb.clone(), port);
        transport.prepare().await.unwrap();
        transport.cleanup().await.unwrap();
        let invocations = adb.recorded();
        assert_eq!(invocations.len(), 2);
        assert_eq!(
            invocations[1],
            vec![
                "reverse".to_string(),
                "--remove".to_string(),
                format!("tcp:{port}"),
            ]
        );
    }

    #[tokio::test]
    async fn failing_adb_reports_the_exit_code() {
        let adb = RecordingAdb::failing(3);
        let mut transport = AdbReverseTransport::with_runner(adb, test_port());
        assert!(matches!(
            transport.prepare().await,
            Err(TransportError::Adb {
                step: "reverse",
                code: Some(3),
                ..
            })
        ));
    }
}
