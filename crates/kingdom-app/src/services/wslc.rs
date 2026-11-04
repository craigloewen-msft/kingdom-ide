//! Transport to containers in WSL's separate runtime VM.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::task::JoinHandle;

struct Forward {
    address: SocketAddr,
    task: JoinHandle<()>,
}

static FORWARDS: OnceLock<Mutex<HashMap<String, Forward>>> = OnceLock::new();

fn forwards() -> std::sync::MutexGuard<'static, HashMap<String, Forward>> {
    FORWARDS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

pub(super) fn selected() -> Result<bool, String> {
    runtime(std::env::var("KINGDOM_CONTAINER_RUNTIME").ok().as_deref())
}

fn runtime(value: Option<&str>) -> Result<bool, String> {
    match value {
        None | Some("docker") => Ok(false),
        Some("wslc") => Ok(true),
        Some(other) => Err(format!(
            "unknown KINGDOM_CONTAINER_RUNTIME `{other}`; use `docker` or `wslc`"
        )),
    }
}

pub(super) fn command() -> Command {
    let mut command = Command::new("wslc");
    command.args(["system", "session", "run"]);
    command.kill_on_drop(true);
    command
}

pub(super) async fn answering(host: &str, port: u16) -> bool {
    // Both values are assigned by Kingdom, never shell input from a manifest.
    let Ok(host) = host.parse::<IpAddr>() else {
        return false;
    };
    matches!(
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            command()
                .args(["sh", "-c", &format!("exec 3<>/dev/tcp/{host}/{port}")])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status(),
        )
        .await,
        Ok(Ok(status)) if status.success()
    )
}

/// A separate port per container, reachable through slirp's non-loopback path.
/// The runtime's bridge addresses are not routes in the user's WSL distro.
pub(super) async fn forward(container: &str, host: &str, port: u16) -> Result<String, String> {
    if let Some(existing) = forwards().get(container) {
        if !existing.task.is_finished() {
            return Ok(existing.address.to_string());
        }
    }

    let socket = UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    // Select the default-route interface without sending a packet.
    socket
        .connect("192.0.2.1:9")
        .map_err(|e| format!("cannot find the WSL distro's network address: {e}"))?;
    let ip = socket.local_addr().map_err(|e| e.to_string())?.ip();
    if ip.is_loopback() || ip.is_unspecified() {
        return Err("wslc needs a non-loopback distro address for isolated plans".into());
    }
    let listener = TcpListener::bind(SocketAddr::new(ip, 0))
        .await
        .map_err(|e| format!("cannot open the wslc service tunnel: {e}"))?;
    let address = listener.local_addr().map_err(|e| e.to_string())?;
    let host = host.to_string();
    let name = container.to_string();
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, peer) = match listener.accept().await {
                Ok(connection) => connection,
                Err(e) => {
                    eprintln!("wslc tunnel for {name} stopped: {e}");
                    break;
                }
            };
            if peer.ip() != address.ip() && !peer.ip().is_loopback() {
                eprintln!("wslc tunnel for {name} refused a non-local connection from {peer}");
                continue;
            }
            let host = host.clone();
            let name = name.clone();
            tokio::spawn(async move {
                let result = async {
                    let mut child = command()
                        .args(["nc", &host, &port.to_string()])
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::inherit())
                        .spawn()?;
                    let mut input = child.stdin.take().expect("piped stdin");
                    let mut output = child.stdout.take().expect("piped stdout");
                    let (mut read, mut write) = stream.split();
                    tokio::try_join!(
                        async {
                            tokio::io::copy(&mut read, &mut input).await?;
                            drop(input);
                            Ok::<(), std::io::Error>(())
                        },
                        async {
                            tokio::io::copy(&mut output, &mut write).await?;
                            write.shutdown().await
                        },
                    )?;
                    Ok::<(), std::io::Error>(())
                }
                .await;
                if let Err(e) = result {
                    eprintln!("wslc tunnel for {name} failed: {e}");
                }
            });
        }
    });
    if let Some(old) = forwards().insert(container.to_string(), Forward { address, task }) {
        old.task.abort();
    }
    Ok(address.to_string())
}

pub(super) fn stop(container: &str) {
    if let Some(forward) = forwards().remove(container) {
        forward.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_remote_runtime_is_opt_in_and_invalid_choices_are_errors() {
        assert_eq!(runtime(None), Ok(false));
        assert_eq!(runtime(Some("docker")), Ok(false));
        assert_eq!(runtime(Some("wslc")), Ok(true));
        assert!(runtime(Some("podman")).unwrap_err().contains("podman"));
        assert!(runtime(Some("")).is_err());
    }

    #[test]
    fn remote_commands_use_the_runtime_session_without_a_shell() {
        let command = command();
        let command = command.as_std();
        assert_eq!(command.get_program(), "wslc");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["system", "session", "run"]
        );
    }
}
