use crate::acceptor::TlsAcceptor;
use crate::config::{load_server_config, DynamicConfig, StaticConfig};
use crate::error::StringError;

use core::task::{Context, Poll};
use hyper::client::{HttpConnector, ResponseFuture};
use hyper::http::uri::Scheme;
use hyper::http::HeaderValue;
use hyper::server::conn::AddrIncoming;
use hyper::service::{make_service_fn, Service};
use hyper::{Body, Client, Request, Response, Server, StatusCode, Uri, Version};
use pin_project::pin_project;
use std::fmt::Display;
use std::future::Ready;
use std::future::{ready, Future};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::{io, sync};
use tokio::net::TcpStream;
use tokio::signal::unix::SignalKind;
use tokio::try_join;

#[tokio::main]
pub async fn run() -> Result<(), StringError> {
	let mut hup_signal = tokio::signal::unix::signal(SignalKind::hangup()).unwrap();

	let static_config = StaticConfig::load()?;

	let dynamic_config = sync::Arc::new(RwLock::new(DynamicConfig::load(&static_config)?));

	let server_config = sync::Arc::new(RwLock::new(load_server_config(
		&dynamic_config.read().unwrap(),
	)?));

	let http_addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, static_config.http_port);
	let https_addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, static_config.https_port);

	let http_incoming = AddrIncoming::bind(&http_addr.into())
		.map_err(|e| StringError::new(format!("Failed to bind address {}: {}", http_addr, e)))?;
	let https_incoming = AddrIncoming::bind(&https_addr.into())
		.map_err(|e| StringError::new(format!("Failed to bind address {}: {}", https_addr, e)))?;

	let client = Client::new();

	let tmp_client = client.clone();
	let tmp_dynamic_config = dynamic_config.clone();
	tokio::spawn(
		Server::builder(http_incoming).serve(make_service_fn(move |_| {
			let client = tmp_client.clone();
			let dynamic_config = tmp_dynamic_config.clone();
			async {
				Ok::<_, io::Error>(Proxy {
					secure: false,
					client: client,
					dynamic_config: dynamic_config,
				})
			}
		})),
	);

	let tmp_client = client.clone();
	let tmp_dynamic_config = dynamic_config.clone();
	tokio::spawn(
		Server::builder(TlsAcceptor::new(server_config.clone(), https_incoming)).serve(
			make_service_fn(move |_| {
				let client = tmp_client.clone();
				let dynamic_config = tmp_dynamic_config.clone();
				async {
					Ok::<_, io::Error>(Proxy {
						secure: true,
						client: client,
						dynamic_config: dynamic_config,
					})
				}
			}),
		),
	);

	println!("Server started");

	tokio::spawn(async move {
		loop {
			hup_signal.recv().await;

			match DynamicConfig::load(&static_config) {
				Ok(new_dynamic_config) => *dynamic_config.write().unwrap() = new_dynamic_config,
				Err(e) => {
					println!("Failed to reload config: {}", e);
					continue;
				}
			}

			match load_server_config(&(dynamic_config.read().unwrap())) {
				Ok(new_server_config) => *server_config.write().unwrap() = new_server_config,
				Err(e) => {
					println!("Failed to reload config: {}", e);
					continue;
				}
			}

			println!("Config reloaded successfully");
		}
	});

	tokio::signal::ctrl_c().await.unwrap();

	println!("Server stopped");

	Ok(())
}

struct Proxy {
	secure: bool,
	client: Client<HttpConnector>,
	dynamic_config: Arc<RwLock<DynamicConfig>>,
}

impl Proxy {
	fn not_found(&self) -> ProxyFuture {
		let status = StatusCode::NOT_FOUND;

		let mut res = Response::new(Body::from(status.to_string()));
		*res.status_mut() = status;

		return ProxyFuture::Ready(ready(Ok(res)));
	}

	fn redirect_to_https(&self, req: &Request<Body>, host: &str) -> ProxyFuture {
		return ProxyFuture::Ready(ready(|| -> Result<Response<Body>, ProxyError> {
			let mut res = Response::new(Body::empty());

			let location = &[
				"https://",
				host,
				req.uri()
					.path_and_query()
					.map_or("", |path_and_query| path_and_query.as_str()),
			]
			.concat();

			*res.status_mut() = StatusCode::MOVED_PERMANENTLY;
			res.headers_mut()
				.append(hyper::header::LOCATION, location.parse::<HeaderValue>()?);

			return Ok(res);
		}()));
	}

	fn proxy(&self, mut req: Request<Body>, host: &str, address: &SocketAddr) -> ProxyFuture {
		*req.version_mut() = Version::HTTP_11;
		*req.uri_mut() = Uri::builder()
			.scheme(Scheme::HTTP)
			.authority(address.to_string())
			.path_and_query(req.uri().path_and_query().unwrap().clone())
			.build()
			.unwrap();

		req.headers_mut().remove("Connection");
		req.headers_mut().remove("Keep-Alive");

		if let Ok(value) = host.parse::<HeaderValue>() {
			req.headers_mut().insert(hyper::header::HOST, value);
		}

		return ProxyFuture::Response(self.client.request(req));
	}

	fn upgrade_proxy(
		&self,
		mut req: Request<Body>,
		host: &str,
		address: &SocketAddr,
	) -> ProxyFuture {
		let mut out_req = Request::new(Body::empty());
		out_req.headers_mut().clone_from(req.headers());
		out_req.method_mut().clone_from(req.method());

		*out_req.version_mut() = Version::HTTP_11;
		*out_req.uri_mut() = Uri::builder()
			.path_and_query(req.uri().path_and_query().unwrap().clone())
			.build()
			.unwrap();

		out_req.headers_mut().remove("Keep-Alive");

		if let Ok(value) = host.parse::<HeaderValue>() {
			req.headers_mut().insert(hyper::header::HOST, value);
		}

		let address = address.clone();

		return ProxyFuture::Boxed(Box::pin(async move {
			let stream = TcpStream::connect(address).await?;
			let (mut sender, conn) = hyper::client::conn::handshake(stream).await?;

			tokio::spawn(conn);

			let mut res = sender.send_request(out_req).await?;

			let mut res_out = Response::new(Body::empty());
			res_out.headers_mut().clone_from(res.headers());
			*res_out.version_mut() = res.version();
			*res_out.status_mut() = res.status();

			tokio::spawn(async move {
				if let Ok((mut res_upgraded, mut req_upgraded)) =
					try_join!(hyper::upgrade::on(&mut res), hyper::upgrade::on(&mut req))
				{
					tokio::io::copy_bidirectional(&mut res_upgraded, &mut req_upgraded)
						.await
						.unwrap();
				}
			});

			return Ok(res_out);
		}));
	}
}

impl Service<Request<Body>> for Proxy {
	type Response = Response<Body>;
	type Error = ProxyError;
	type Future = ProxyFuture;

	fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		Poll::Ready(Ok(()))
	}

	fn call(&mut self, req: Request<Body>) -> Self::Future {
		if let Some(host) = req
			.uri()
			.authority()
			.map(|authority| authority.as_str())
			.or_else(|| {
				req.headers()
					.get(hyper::header::HOST)
					.map_or(Some(""), |host| host.to_str().ok())
			}) {
			if !self.secure {
				return self.redirect_to_https(&req, host);
			}

			if let Some(route) = self
				.dynamic_config
				.read()
				.unwrap()
				.routes
				.iter()
				.find(|route| route.host.matches(host))
			{
				let host = host.to_string();
				if req.headers().contains_key(hyper::header::UPGRADE) {
					return self.upgrade_proxy(req, host.as_str(), &route.address);
				} else {
					return self.proxy(req, host.as_str(), &route.address);
				}
			}
		}

		return self.not_found();
	}
}

#[derive(Debug)]
struct ProxyError {}

impl std::error::Error for ProxyError {}

impl Display for ProxyError {
	fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		Ok(())
	}
}

impl From<hyper::header::InvalidHeaderValue> for ProxyError {
	fn from(_: hyper::header::InvalidHeaderValue) -> Self {
		return Self {};
	}
}

impl From<hyper::Error> for ProxyError {
	fn from(_: hyper::Error) -> Self {
		return Self {};
	}
}

impl From<io::Error> for ProxyError {
	fn from(_: io::Error) -> Self {
		return Self {};
	}
}

#[pin_project(project = ProxyFutureProj)]
enum ProxyFuture {
	Response(#[pin] ResponseFuture),
	Boxed(#[pin] Pin<Box<dyn Future<Output = Result<Response<Body>, ProxyError>> + Send + Sync>>),
	Ready(#[pin] Ready<Result<Response<Body>, ProxyError>>),
}

impl Future for ProxyFuture {
	type Output = Result<Response<Body>, ProxyError>;

	fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		match self.project() {
			ProxyFutureProj::Response(response) => {
				return response.poll(cx).map_err(|e| e.into());
			}
			ProxyFutureProj::Boxed(boxed) => {
				return boxed.poll(cx);
			}
			ProxyFutureProj::Ready(ready) => {
				return ready.poll(cx);
			}
		}
	}
}
