//! No environment, HTTP bearer, TCP or database configuration is read.
#[cfg(unix)]
fn check() -> std::io::Result<()> {
    use std::io::{BufRead, BufReader, Write};
    let path = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "/run/agentforge-sandbox/control.sock".into());
    if !path.is_absolute() {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    let mut socket = std::os::unix::net::UnixStream::connect(path)?;
    socket.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
    socket.set_write_timeout(Some(std::time::Duration::from_secs(2)))?;
    socket.write_all(
        b"GET /health/ready HTTP/1.1\r\nHost: sandbox.invalid\r\nConnection: close\r\n\r\n",
    )?;
    let mut status = String::new();
    use std::io::Read;
    BufReader::new(socket).take(128).read_line(&mut status)?;
    if status.starts_with("HTTP/1.1 200 ") || status.starts_with("HTTP/1.0 200 ") {
        Ok(())
    } else {
        Err(std::io::ErrorKind::NotConnected.into())
    }
}
fn main() -> std::process::ExitCode {
    #[cfg(unix)]
    if check().is_ok() {
        return std::process::ExitCode::SUCCESS;
    }
    std::process::ExitCode::FAILURE
}
