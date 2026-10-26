//! IMAP login probe: waits until the mail server accepts a new password.
//!
//! docker-mailserver applies an edited account file a few seconds after the write. If Nextcloud
//! Mail logs in during that window, the failure counts against Nextcloud's brute-force
//! protection, and the retries that follow are throttled (HTTP 429). Probing IMAP directly
//! avoids Nextcloud entirely.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use pcnc_core::backend::{Endpoint, Security};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::{Instant, sleep, timeout};
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore, crypto};

const IO_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct ImapProbe {
    pub endpoint: Endpoint,
    /// Give up after this long.
    pub timeout: Duration,
    /// Pause between attempts.
    pub interval: Duration,
}

impl ImapProbe {
    pub fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            timeout: Duration::from_secs(90),
            interval: Duration::from_secs(1),
        }
    }

    /// Returns once `user` can log in with `password`.
    pub async fn wait(&self, user: &str, password: &str) -> anyhow::Result<()> {
        let deadline = Instant::now() + self.timeout;
        loop {
            let last = match self.try_login(user, password).await {
                Ok(true) => return Ok(()),
                Ok(false) => "login refused".to_string(),
                Err(e) => format!("{e:#}"),
            };
            if Instant::now() >= deadline {
                bail!(
                    "{}:{} did not accept the new password for {user} within {:?} ({last})",
                    self.endpoint.host,
                    self.endpoint.port,
                    self.timeout
                );
            }
            sleep(self.interval).await;
        }
    }

    /// One attempt: `Ok(true)` logged in, `Ok(false)` refused, `Err` could not ask.
    pub async fn try_login(&self, user: &str, password: &str) -> anyhow::Result<bool> {
        let ep = &self.endpoint;
        let tcp = timeout(IO_TIMEOUT, TcpStream::connect((ep.host.as_str(), ep.port)))
            .await
            .context("connect timed out")?
            .with_context(|| format!("connecting to {}:{}", ep.host, ep.port))?;
        match ep.security {
            Security::None => login(tcp, true, user, password).await,
            Security::Ssl => login(tls(&ep.host, tcp).await?, true, user, password).await,
            Security::Starttls => {
                let mut io = BufReader::new(tcp);
                expect_greeting(&mut io).await?;
                io.get_mut().write_all(b"s1 STARTTLS\r\n").await?;
                if !tagged(&mut io, "s1").await? {
                    bail!("STARTTLS refused");
                }
                login(tls(&ep.host, io.into_inner()).await?, false, user, password).await
            }
        }
    }
}

async fn tls(
    host: &str,
    tcp: TcpStream,
) -> anyhow::Result<tokio_rustls::client::TlsStream<TcpStream>> {
    let roots = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let config = ClientConfig::builder_with_provider(Arc::new(crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = ServerName::try_from(host.to_string()).context("invalid TLS server name")?;
    timeout(
        IO_TIMEOUT,
        TlsConnector::from(Arc::new(config)).connect(name, tcp),
    )
    .await
    .context("TLS handshake timed out")?
    .context("TLS handshake")
}

async fn read_line<S: AsyncRead + Unpin>(io: &mut BufReader<S>) -> anyhow::Result<String> {
    let mut line = String::new();
    let n = timeout(IO_TIMEOUT, io.read_line(&mut line))
        .await
        .context("read timed out")??;
    if n == 0 {
        bail!("connection closed");
    }
    Ok(line)
}

async fn expect_greeting<S: AsyncRead + Unpin>(io: &mut BufReader<S>) -> anyhow::Result<()> {
    let line = read_line(io).await?;
    if !line.starts_with("* OK") {
        bail!("unexpected IMAP greeting: {}", line.trim());
    }
    Ok(())
}

/// Reads until the tagged response for `tag`; true for OK.
async fn tagged<S: AsyncRead + Unpin>(io: &mut BufReader<S>, tag: &str) -> anyhow::Result<bool> {
    loop {
        let line = read_line(io).await?;
        if let Some(rest) = line.strip_prefix(tag).and_then(|r| r.strip_prefix(' ')) {
            return Ok(rest.starts_with("OK"));
        }
    }
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

async fn login<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    greeting: bool,
    user: &str,
    password: &str,
) -> anyhow::Result<bool> {
    let mut io = BufReader::new(stream);
    if greeting {
        expect_greeting(&mut io).await?;
    }
    let cmd = format!("a1 LOGIN {} {}\r\n", quote(user), quote(password));
    io.get_mut().write_all(cmd.as_bytes()).await?;
    let ok = tagged(&mut io, "a1").await?;
    let _ = io.get_mut().write_all(b"a2 LOGOUT\r\n").await;
    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    /// A tiny IMAP server: accepts `user`/`good` only after `refusals` failed attempts.
    async fn fake_imap(refusals: usize) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut seen = 0;
            loop {
                let (sock, _) = listener.accept().await.unwrap();
                let mut io = BufReader::new(sock);
                io.get_mut()
                    .write_all(b"* OK [CAPABILITY IMAP4rev1] ready\r\n")
                    .await
                    .unwrap();
                let mut line = String::new();
                io.read_line(&mut line).await.unwrap();
                let good =
                    line == "a1 LOGIN \"user@example.test\" \"go\\\"od\"\r\n" && seen >= refusals;
                seen += 1;
                let reply: &[u8] = if good {
                    b"* CAPABILITY IMAP4rev1\r\na1 OK Logged in\r\n"
                } else {
                    b"a1 NO [AUTHENTICATIONFAILED] nope\r\n"
                };
                io.get_mut().write_all(reply).await.unwrap();
            }
        });
        port
    }

    fn probe(port: u16) -> ImapProbe {
        ImapProbe {
            endpoint: Endpoint {
                host: "127.0.0.1".into(),
                port,
                security: Security::None,
            },
            timeout: Duration::from_secs(2),
            interval: Duration::from_millis(20),
        }
    }

    #[tokio::test]
    async fn waits_until_the_login_is_accepted() {
        let p = probe(fake_imap(3).await);
        p.wait("user@example.test", "go\"od").await.unwrap();
    }

    #[tokio::test]
    async fn gives_up_on_a_wrong_password() {
        let p = probe(fake_imap(0).await);
        let err = p.wait("user@example.test", "bad").await.unwrap_err();
        assert!(format!("{err}").contains("did not accept"), "{err}");
    }

    #[tokio::test]
    async fn reports_unreachable_servers() {
        let p = probe(1);
        assert!(p.try_login("u", "p").await.is_err());
    }
}
