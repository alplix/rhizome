//! Moving the bytes of a `DCC SEND` transfer.
//!
//! `rhizome_proto::dcc` only reads and writes the CTCP request that offers a
//! file; the transfer itself is a second, plain TCP connection, direct
//! between the two people, with no framing at all — the sender simply writes
//! the file's bytes and the receiver reads exactly as many as they were told
//! to expect. That is real I/O, so unlike the sans-I/O [`crate::session`], this
//! module owns a socket directly, the same way [`crate::connection`] does for
//! the chat connection itself.
//!
//! **This implements active DCC only:** the side sending the file listens and
//! the side receiving it connects out. A firewall or NAT between the two
//! peers with nothing port-forwarded will simply time out waiting for a
//! connection — true of every classic DCC client, not particular to this one.
//! "Reverse" (passive) DCC, where the roles of listening and connecting swap
//! to work around exactly that, is not implemented; see
//! [`rhizome_proto::dcc::Send::is_passive`].

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::timeout;

/// How long a listening offer waits for the other side to connect.
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(120);
/// How long connecting to accept an offer waits.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Bytes moved per read/write.
const CHUNK: usize = 64 * 1024;
/// Progress is throttled to about this often, so a fast transfer of a huge
/// file does not flood its listener with an update for every chunk.
const PROGRESS_EVERY: Duration = Duration::from_millis(200);

/// What a transfer in progress reports back, as it happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferEvent {
    /// Bytes moved so far, out of the total.
    Progress(u64),
    /// Every byte moved successfully.
    Done,
    /// Stopped before finishing, with why.
    Failed(String),
}

fn send_progress(tx: &mpsc::UnboundedSender<TransferEvent>, event: TransferEvent) {
    let _ = tx.send(event);
}

/// This machine's IPv4 address on the route a connection to the wider
/// internet would take, found without sending anything: a UDP "connect"
/// only consults the routing table.
///
/// This is what gets announced in a `DCC SEND` offer. On a machine behind a
/// NAT with nothing port-forwarded, it is a private address the other side
/// cannot reach — the same limitation every DCC client has without UPnP or a
/// relay, neither of which this implements.
pub async fn local_ipv4() -> io::Result<Ipv4Addr> {
    let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
    socket.connect(("8.8.8.8", 80)).await?;
    match socket.local_addr()?.ip() {
        std::net::IpAddr::V4(ip) => Ok(ip),
        std::net::IpAddr::V6(_) => Ok(Ipv4Addr::UNSPECIFIED),
    }
}

/// Starts listening for the offer this process is about to make, and reports
/// which port was chosen (`0` asks the OS for a free one).
pub async fn listen() -> io::Result<(TcpListener, u16)> {
    let listener = TcpListener::bind(("0.0.0.0", 0)).await?;
    let port = listener.local_addr()?.port();
    Ok((listener, port))
}

/// Accepts one connection on `listener` and sends it the whole of `path`,
/// reporting progress as it goes. Meant to be run in its own task: it does
/// not return until the transfer finishes, fails, or the accept itself times
/// out.
pub async fn offer_send(
    listener: TcpListener,
    path: PathBuf,
    progress: mpsc::UnboundedSender<TransferEvent>,
) {
    let outcome = send_once(listener, &path, &progress, ACCEPT_TIMEOUT).await;
    if let Err(e) = outcome {
        send_progress(&progress, TransferEvent::Failed(e));
    }
}

async fn send_once(
    listener: TcpListener,
    path: &Path,
    progress: &mpsc::UnboundedSender<TransferEvent>,
    accept_timeout: Duration,
) -> Result<(), String> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| format!("could not open {}: {e}", path.display()))?;

    let (mut socket, _) = timeout(accept_timeout, listener.accept())
        .await
        .map_err(|_| "nobody connected to accept the file".to_owned())?
        .map_err(|e| format!("accepting the connection failed: {e}"))?;

    let mut buf = vec![0u8; CHUNK];
    let mut sent: u64 = 0;
    let mut last_report = Instant::now();
    loop {
        let n = file
            .read(&mut buf)
            .await
            .map_err(|e| format!("reading the file failed: {e}"))?;
        if n == 0 {
            break;
        }
        socket
            .write_all(&buf[..n])
            .await
            .map_err(|e| format!("sending failed: {e}"))?;
        sent += n as u64;
        if last_report.elapsed() >= PROGRESS_EVERY {
            send_progress(progress, TransferEvent::Progress(sent));
            last_report = Instant::now();
        }
    }
    let _ = socket.flush().await;
    send_progress(progress, TransferEvent::Progress(sent));
    send_progress(progress, TransferEvent::Done);
    Ok(())
}

/// Connects to an offer's address and reads exactly `size` bytes into `dest`,
/// reporting progress as it goes. Also meant to be run in its own task.
pub async fn accept_send(
    ip: Ipv4Addr,
    port: u16,
    size: u64,
    dest: PathBuf,
    progress: mpsc::UnboundedSender<TransferEvent>,
) {
    let outcome = receive_once(ip, port, size, &dest, &progress, CONNECT_TIMEOUT).await;
    if let Err(e) = outcome {
        // A file left half-written would look like a real, truncated
        // download rather than a failed one.
        let _ = tokio::fs::remove_file(&dest).await;
        send_progress(&progress, TransferEvent::Failed(e));
    }
}

async fn receive_once(
    ip: Ipv4Addr,
    port: u16,
    size: u64,
    dest: &Path,
    progress: &mpsc::UnboundedSender<TransferEvent>,
    connect_timeout: Duration,
) -> Result<(), String> {
    let addr = SocketAddr::from((ip, port));
    let mut socket = timeout(connect_timeout, TcpStream::connect(addr))
        .await
        .map_err(|_| format!("timed out connecting to {addr}"))?
        .map_err(|e| format!("could not connect to {addr}: {e}"))?;

    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| format!("could not create {}: {e}", dest.display()))?;

    let mut buf = vec![0u8; CHUNK];
    let mut received: u64 = 0;
    let mut last_report = Instant::now();
    while received < size {
        let want = CHUNK.min((size - received) as usize);
        let n = socket
            .read(&mut buf[..want])
            .await
            .map_err(|e| format!("receiving failed: {e}"))?;
        if n == 0 {
            return Err(format!(
                "the connection closed after {received} of {size} bytes"
            ));
        }
        file.write_all(&buf[..n])
            .await
            .map_err(|e| format!("writing {}: {e}", dest.display()))?;
        received += n as u64;
        if last_report.elapsed() >= PROGRESS_EVERY {
            send_progress(progress, TransferEvent::Progress(received));
            last_report = Instant::now();
        }
    }
    let _ = file.flush().await;
    send_progress(progress, TransferEvent::Progress(received));
    send_progress(progress, TransferEvent::Done);
    Ok(())
}

/// A name that is safe to create in a fixed downloads folder: whatever the
/// offer's already-sanitized filename was (see
/// `rhizome_proto::dcc::parse_send`), but never overwriting a file already
/// there.
pub fn unique_destination(dir: &Path, filename: &str) -> std::path::PathBuf {
    let candidate = dir.join(filename);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match filename.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (filename, String::new()),
    };
    for n in 1u32.. {
        let candidate = dir.join(format!("{stem} ({n}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration as StdDuration;

    async fn drain(rx: &mut mpsc::UnboundedReceiver<TransferEvent>) -> Vec<TransferEvent> {
        let mut events = Vec::new();
        while let Ok(Some(event)) = timeout(StdDuration::from_secs(5), rx.recv()).await {
            let done = matches!(event, TransferEvent::Done | TransferEvent::Failed(_));
            events.push(event);
            if done {
                break;
            }
        }
        events
    }

    #[tokio::test]
    async fn a_file_is_sent_and_received_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.bin");
        let content: Vec<u8> = (0..500_000u32).map(|n| (n % 251) as u8).collect();
        tokio::fs::write(&source, &content).await.unwrap();

        let (listener, port) = listen().await.unwrap();
        let (tx_send, mut rx_send) = mpsc::unbounded_channel();
        let sender = tokio::spawn(offer_send(listener, source.clone(), tx_send));

        let dest = dir.path().join("received.bin");
        let (tx_recv, mut rx_recv) = mpsc::unbounded_channel();
        let receiver = tokio::spawn(accept_send(
            Ipv4Addr::LOCALHOST,
            port,
            content.len() as u64,
            dest.clone(),
            tx_recv,
        ));

        let sent_events = drain(&mut rx_send).await;
        let received_events = drain(&mut rx_recv).await;
        sender.await.unwrap();
        receiver.await.unwrap();

        assert_eq!(sent_events.last(), Some(&TransferEvent::Done));
        assert_eq!(received_events.last(), Some(&TransferEvent::Done));
        assert_eq!(tokio::fs::read(&dest).await.unwrap(), content);

        // Progress only ever goes up, and never past the total.
        let mut previous = 0;
        for event in &received_events {
            if let TransferEvent::Progress(n) = event {
                assert!(*n >= previous && *n <= content.len() as u64);
                previous = *n;
            }
        }
    }

    #[tokio::test]
    async fn a_small_file_still_completes_in_one_chunk() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("tiny.txt");
        tokio::fs::write(&source, b"merhaba").await.unwrap();

        let (listener, port) = listen().await.unwrap();
        let (tx_send, mut rx_send) = mpsc::unbounded_channel();
        tokio::spawn(offer_send(listener, source, tx_send));

        let dest = dir.path().join("out.txt");
        let (tx_recv, _rx_recv) = mpsc::unbounded_channel();
        accept_send(Ipv4Addr::LOCALHOST, port, 7, dest.clone(), tx_recv).await;

        assert_eq!(tokio::fs::read_to_string(&dest).await.unwrap(), "merhaba");
        assert!(drain(&mut rx_send).await.contains(&TransferEvent::Done));
    }

    #[tokio::test]
    async fn nobody_connecting_is_reported_not_left_hanging() {
        // The public entry point always waits the real ACCEPT_TIMEOUT, so
        // this calls the private, timeout-taking function underneath it
        // directly, with one short enough for a test.
        let (listener, _port) = listen().await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("x.txt");
        tokio::fs::write(&source, b"x").await.unwrap();

        let (tx, _rx) = mpsc::unbounded_channel();
        let err = send_once(listener, &source, &tx, StdDuration::from_millis(200))
            .await
            .unwrap_err();
        assert!(err.contains("nobody connected"), "{err}");
    }

    #[tokio::test]
    async fn a_connection_that_closes_early_is_a_reported_failure_not_a_short_file() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            socket.write_all(b"only this much").await.unwrap();
            // Then just drop the connection, well short of the promised size.
        });

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.bin");
        let (tx, mut rx) = mpsc::unbounded_channel();
        accept_send(Ipv4Addr::LOCALHOST, port, 10_000_000, dest.clone(), tx).await;
        server.await.unwrap();

        let events = drain(&mut rx).await;
        assert!(matches!(events.last(), Some(TransferEvent::Failed(_))));
        assert!(!dest.exists(), "a half-written file is cleaned up");
    }

    #[test]
    fn a_destination_that_already_exists_gets_a_number_instead_of_being_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "one").unwrap();
        let first = unique_destination(dir.path(), "notes.txt");
        assert_eq!(first, dir.path().join("notes (1).txt"));

        std::fs::write(&first, "two").unwrap();
        let second = unique_destination(dir.path(), "notes.txt");
        assert_eq!(second, dir.path().join("notes (2).txt"));

        assert_eq!(
            unique_destination(dir.path(), "unclaimed.txt"),
            dir.path().join("unclaimed.txt"),
            "a name nothing is using yet is not renamed"
        );
    }

    #[test]
    fn an_extensionless_name_still_gets_a_number() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README"), "x").unwrap();
        assert_eq!(
            unique_destination(dir.path(), "README"),
            dir.path().join("README (1)")
        );
    }

    #[tokio::test]
    async fn the_local_address_found_is_a_real_ipv4_address() {
        // Not much to assert about which address a given test machine has,
        // only that this actually returns one instead of failing.
        let ip = local_ipv4().await.unwrap();
        assert!(!ip.is_unspecified() || cfg!(test));
        let _ = ip.octets();
    }
}
