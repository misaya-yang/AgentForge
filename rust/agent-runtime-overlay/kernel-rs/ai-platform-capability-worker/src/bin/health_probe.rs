use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

fn main() -> std::io::Result<()> {
    let address: SocketAddr = std::env::var("AI_PLATFORM_CAPABILITY_WORKER_HEALTH_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8095".to_string())
        .parse()
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let timeout = Duration::from_secs(2);
    let mut stream = TcpStream::connect_timeout(&address, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream
        .write_all(b"GET /health/ready HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
    let mut response = [0_u8; 256];
    let length = stream.read(&mut response)?;
    if response[..length].starts_with(b"HTTP/1.1 200") {
        Ok(())
    } else {
        Err(std::io::Error::other("capability worker is not ready"))
    }
}
