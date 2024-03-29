use std::{future::Future, io, sync::Arc};

use fastwebsockets::FragmentCollector;
use http_body_util::Empty;
use hyper::{
    body::Bytes,
    header::{CONNECTION, UPGRADE},
    upgrade::Upgraded,
    Request,
};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tokio_rustls::{rustls::ClientConfig, TlsConnector};
use url::Url;

#[derive(thiserror::Error, Debug)]
pub enum StartError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Http(#[from] http::Error),
    #[error("{0}")]
    Fast(#[from] fastwebsockets::WebSocketError),
    #[error("{0}")]
    InvalidUrl(#[from] url::ParseError),
    #[error("There was no host in the provided URL")]
    NoHost,
    #[error("There was no domain in the provided URL")]
    NoDomain,
}

pub type Socket = FragmentCollector<TokioIo<Upgraded>>;

pub async fn connect(url: &Url) -> Result<Socket, StartError> {
    let Some(host) = url.host_str() else {
        return Err(StartError::NoHost);
    };
    let Some(domain) = url.domain() else {
        return Err(StartError::NoDomain);
    };
    let use_tls = url.scheme() == "wss";
    let port = url.port().unwrap_or(if use_tls { 443 } else { 80 });

    let tcp_stream = TcpStream::connect((host, port)).await?;

    let ws = if use_tls {
        let tls_connector = tls_connector();
        let domain = tokio_rustls::rustls::pki_types::ServerName::try_from(domain)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid dnsname"))?
            .to_owned();

        let tls_stream = tls_connector.connect(domain, tcp_stream).await?;

        fastwebsockets::handshake::client(&SpawnExecutor, make_request(url, host)?, tls_stream)
            .await?
            .0
    } else {
        fastwebsockets::handshake::client(&SpawnExecutor, make_request(url, host)?, tcp_stream)
            .await?
            .0
    };

    Ok(FragmentCollector::new(ws))
}

struct SpawnExecutor;

impl<Fut> hyper::rt::Executor<Fut> for SpawnExecutor
where
    Fut: Future + Send + 'static,
    Fut::Output: Send + 'static,
{
    fn execute(&self, fut: Fut) {
        tokio::task::spawn(fut);
    }
}

fn tls_connector() -> TlsConnector {
    let root_store = tokio_rustls::rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };

    let config = ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    TlsConnector::from(Arc::new(config))
}

fn make_request(url: &Url, host: &str) -> Result<Request<Empty<Bytes>>, http::Error> {
    Request::builder()
        .method("GET")
        .uri(url.as_str())
        .header("Host", host)
        .header(UPGRADE, "websocket")
        .header(CONNECTION, "upgrade")
        .header(
            "Sec-WebSocket-Key",
            fastwebsockets::handshake::generate_key(),
        )
        .header("Sec-WebSocket-Version", "13")
        .body(Empty::<Bytes>::new())
}
