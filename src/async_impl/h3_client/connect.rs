use crate::async_impl::h3_client::dns::resolve;
use crate::dns::DynResolver;
use crate::error::BoxError;
use bytes::Bytes;
use h3::client::SendRequest;
use h3_quinn::{Connection, OpenStreams};
use http::Uri;
use hyper_util::client::legacy::connect::dns::Name;
use quinn::crypto::rustls::QuicClientConfig;
use quinn::{ClientConfig, Endpoint, TransportConfig};
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::Arc;

type H3Connection = (
    h3::client::Connection<Connection, Bytes>,
    SendRequest<OpenStreams, Bytes>,
);

#[derive(Clone)]
pub(crate) struct H3Connector {
    resolver: DynResolver,
    endpoint_ipv4: Endpoint,
    endpoint_ipv6: Endpoint,
}

impl H3Connector {
    pub fn new(
        resolver: DynResolver,
        tls: rustls::ClientConfig,
        local_addr: Option<IpAddr>,
        transport_config: TransportConfig,
    ) -> Result<H3Connector, BoxError> {
        let quic_client_config = Arc::new(QuicClientConfig::try_from(tls)?);
        let mut config = ClientConfig::new(quic_client_config);
        // FIXME: Replace this when there is a setter.
        config.transport_config(Arc::new(transport_config));

        /*
        let socket_addr = match local_addr {
            Some(ip) => SocketAddr::new(ip, 0),
            None => "[::]:0".parse::<SocketAddr>().unwrap(),
        };
        */

        let mut endpoint_ipv4 = Endpoint::client("0.0.0.0:0".parse().unwrap())?;
        endpoint_ipv4.set_default_client_config(config.clone());
        endpoint_ipv4.rebind(std::net::UdpSocket::bind("0.0.0.0:0")?)?;

        let mut endpoint_ipv6 = Endpoint::client("[::]:0".parse().unwrap())?;
        endpoint_ipv6.set_default_client_config(config);
        endpoint_ipv6.rebind(std::net::UdpSocket::bind("[::]:0")?)?;

        Ok(Self { resolver, endpoint_ipv4, endpoint_ipv6 })
    }

    pub async fn connect(&mut self, dest: Uri) -> Result<H3Connection, BoxError> {
        let host = dest
            .host()
            .ok_or("destination must have a host")?
            .trim_start_matches('[')
            .trim_end_matches(']');
        let port = dest.port_u16().unwrap_or(443);

        let addrs = if let Some(addr) = IpAddr::from_str(host).ok() {
            // If the host is already an IP address, skip resolving.
            vec![SocketAddr::new(addr, port)]
        } else {
            let addrs = resolve(&mut self.resolver, Name::from_str(host)?).await?;
            let addrs = addrs.map(|mut addr| {
                addr.set_port(port);
                addr
            });
            addrs.collect()
        };

        self.remote_connect(addrs, host).await
    }

    async fn remote_connect(
        &mut self,
        addrs: Vec<SocketAddr>,
        server_name: &str,
    ) -> Result<H3Connection, BoxError> {
        let mut err = None;
        for addr in addrs {
            let endpoint =
                if addr.is_ipv4() {
                    &self.endpoint_ipv4
                } else {
                    &self.endpoint_ipv6
                };
            match endpoint.connect(addr, server_name)?.await {
                Ok(new_conn) => {
                    let quinn_conn = Connection::new(new_conn);
                    return Ok(h3::client::new(quinn_conn).await?);
                }
                Err(e) => err = Some(e),
            }
        }

        match err {
            Some(e) => Err(Box::new(e) as BoxError),
            None => Err("failed to establish connection for HTTP/3 request".into()),
        }
    }
}
