//! The HTTP client of model calls and downloads: ureq, connecting to a loopback name such as
//! `localhost` through the address that answers.
//!
//! ureq tries the addresses a name resolves to one after another. `localhost` resolves to `::1`
//! first, and Ollama listens on 127.0.0.1 only. Windows retries a refused connection for about
//! 2 seconds, so every new connection to `http://localhost:11434` waited 2 seconds before ureq
//! tried 127.0.0.1. Here, when every address of the host is a loopback address, they are all
//! tried at once and ureq connects to the first that accepts. A server that listens on `::1`
//! only is still reached through `::1`. Other hosts connect as ureq does by default.

use std::net::{SocketAddr, TcpStream};
use std::sync::mpsc;
use std::time::Duration;

use ureq::config::Config;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs};
use ureq::unversioned::transport::{
    ConnectProxyConnector, ConnectionDetails, Connector, NextTimeout, RustlsConnector,
    TcpConnector, Transport,
};

/// An agent with ureq's default connectors, except that a loopback name with several addresses
/// connects through the address that answers first.
pub(crate) fn agent(config: Config) -> ureq::Agent {
    let connector =
        ().chain(ConnectProxyConnector::default())
            .chain(LoopbackConnector::default())
            .chain(RustlsConnector::default());
    ureq::Agent::with_parts(config, connector, DefaultResolver::default())
}

/// [`TcpConnector`], which first moves the loopback address that accepts to the front.
#[derive(Debug, Default)]
struct LoopbackConnector(TcpConnector);

impl<In: Transport> Connector<In> for LoopbackConnector {
    type Out = <TcpConnector as Connector<In>>::Out;

    fn connect(
        &self,
        details: &ConnectionDetails,
        chained: Option<In>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        let addrs = &details.addrs;
        if chained.is_some()
            || details.config.proxy().is_some()
            || addrs.len() < 2
            || !addrs.iter().all(|addr| addr.ip().is_loopback())
        {
            return self.0.connect(details, chained);
        }
        let first = first_accepting(addrs, details.timeout)?;
        let mut ordered = ResolvedSocketAddrs::from_fn(|_| first);
        ordered.push(first);
        for addr in addrs.iter().filter(|addr| **addr != first) {
            ordered.push(*addr);
        }
        let details = ConnectionDetails {
            addrs: ordered,
            current_time: details.current_time.clone(),
            run_connector: details.run_connector.clone(),
            register_shutdown: details.register_shutdown.clone(),
            ..*details
        };
        self.0.connect(&details, None)
    }
}

/// The first of `addrs` that accepts a connection, all tried at once. The trial connections are
/// closed; a loopback connection costs next to nothing.
fn first_accepting(addrs: &[SocketAddr], timeout: NextTimeout) -> Result<SocketAddr, ureq::Error> {
    let wait = timeout
        .not_zero()
        .map(|after| (*after).max(Duration::from_millis(1)));
    let (sender, receiver) = mpsc::channel();
    for &addr in addrs {
        let sender = sender.clone();
        std::thread::spawn(move || {
            let result = match wait {
                Some(wait) => TcpStream::connect_timeout(&addr, wait),
                None => TcpStream::connect(addr),
            };
            // The receiver is gone once another address has accepted.
            let _ = sender.send(result.map(|_| addr));
        });
    }
    drop(sender);
    let mut last_error = None;
    for result in receiver {
        match result {
            Ok(addr) => return Ok(addr),
            Err(error) => last_error = Some(error),
        }
    }
    Err(match last_error {
        Some(error) if error.kind() == std::io::ErrorKind::TimedOut => {
            ureq::Error::Timeout(timeout.reason)
        }
        Some(error) => ureq::Error::Io(error),
        None => ureq::Error::ConnectionFailed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Instant;

    // An IPv4-only server reached through `localhost`, as Ollama is: embed::tests.

    #[test]
    fn reaches_an_ipv6_only_server_through_localhost() {
        // Machines without IPv6 cannot listen on ::1; there is nothing to check there.
        let Ok(listener) = TcpListener::bind("[::1]:0") else {
            return;
        };
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut request = [0; 4096];
                // Trial connections close without a request.
                if stream.read(&mut request).unwrap_or(0) == 0 {
                    continue;
                }
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                );
            }
        });
        let started = Instant::now();
        let text = agent(Config::default())
            .get(&format!("http://localhost:{port}/"))
            .call()
            .unwrap()
            .body_mut()
            .read_to_string()
            .unwrap();
        let took = started.elapsed();
        assert_eq!(text, "ok");
        assert!(took < Duration::from_millis(500), "took {took:?}");
    }

    #[test]
    fn reports_refusal_when_no_address_answers() {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let error = agent(Config::default())
            .get(&format!("http://localhost:{port}/"))
            .call()
            .unwrap_err();
        assert!(
            matches!(&error, ureq::Error::Io(error) if error.kind() == std::io::ErrorKind::ConnectionRefused),
            "{error:?}"
        );
    }
}
