use crate::config::DynamicConfig;

use anyhow::{Error, Result};
use core::task::{Context, Poll};
use http_body_util::{Empty, Full};
use hyper::{
	body::{Body, Bytes, Frame, Incoming, SizeHint},
	client::conn::http1,
	header,
	http::HeaderValue,
	service::Service,
	HeaderMap, Request, Response, StatusCode, Uri, Version,
};
use hyper_util::rt::TokioIo;
use itertools::Itertools;
use pin_project::pin_project;
use std::{
	future::{ready, Future, Ready},
	net::SocketAddr,
	pin::Pin,
	sync::{Arc, RwLock},
};
use tokio::{net::TcpStream, try_join};

pub struct Proxy {
	secure: bool,
	dynamic_config: Arc<RwLock<DynamicConfig>>,
}

impl Proxy {
	pub fn new_secure(dynamic_config: Arc<RwLock<DynamicConfig>>) -> Self {
		Self {
			dynamic_config,
			secure: true,
		}
	}

	pub fn new_unsecure(dynamic_config: Arc<RwLock<DynamicConfig>>) -> Self {
		Self {
			dynamic_config,
			secure: false,
		}
	}

	fn merge_cookie_headers(&self, map: &mut HeaderMap) {
		map.insert(
			header::COOKIE,
			HeaderValue::try_from(
				itertools::Itertools::intersperse(
					map.get_all(header::COOKIE)
						.into_iter()
						.map(|header| header.as_bytes()),
					b"; ",
				)
				.flatten()
				.cloned()
				.collect::<Vec<u8>>(),
			)
			.unwrap(),
		);
	}

	fn not_found(&self) -> ProxyFuture {
		return ProxyFuture::Ready(ready({
			let status = StatusCode::NOT_FOUND;

			let mut out_res = Response::new(ProxyBody::Full(Full::from(status.to_string())));
			*out_res.status_mut() = status;

			Ok(out_res)
		}));
	}

	fn redirect_to_https(&self, in_req: &Request<Incoming>, host: &str) -> ProxyFuture {
		return ProxyFuture::Ready(ready(|| -> Result<_> {
			let mut out_res = Response::new(ProxyBody::Empty(Empty::new()));

			let location = &[
				"https://",
				host,
				in_req
					.uri()
					.path_and_query()
					.map_or("", |path_and_query| path_and_query.as_str()),
			]
			.concat();

			*out_res.status_mut() = StatusCode::MOVED_PERMANENTLY;
			out_res
				.headers_mut()
				.append(header::LOCATION, location.parse::<HeaderValue>()?);

			Ok(out_res)
		}()));
	}

	fn forward(&self, in_req: Request<Incoming>, host: &str, address: &SocketAddr) -> ProxyFuture {
		let mut out_req = in_req;
		*out_req.version_mut() = Version::HTTP_11;
		*out_req.uri_mut() = Uri::builder()
			.path_and_query(out_req.uri().path_and_query().unwrap().clone())
			.build()
			.unwrap();

		out_req.headers_mut().remove("Keep-Alive");
		out_req.headers_mut().remove("Connection");
		out_req.headers_mut().remove("Upgrade");

		if let Ok(value) = host.parse::<HeaderValue>() {
			out_req.headers_mut().insert(header::HOST, value);
		}

		self.merge_cookie_headers(out_req.headers_mut());

		let address = *address;
		return ProxyFuture::Boxed(Box::pin(async move {
			let stream = TcpStream::connect(address).await?;
			let (mut sender, conn) = http1::handshake(TokioIo::new(stream)).await?;

			tokio::spawn(conn);

			let in_res = sender.send_request(out_req).await?;
			let out_res = in_res.map(ProxyBody::Incoming);

			Ok(out_res)
		}));
	}

	fn upgrade(&self, in_req: Request<Incoming>, host: &str, address: &SocketAddr) -> ProxyFuture {
		let mut out_req = Request::new(Empty::<Bytes>::new());
		out_req.headers_mut().clone_from(in_req.headers());
		out_req.method_mut().clone_from(in_req.method());

		*out_req.version_mut() = Version::HTTP_11;
		*out_req.uri_mut() = Uri::builder()
			.path_and_query(in_req.uri().path_and_query().unwrap().clone())
			.build()
			.unwrap();

		out_req.headers_mut().remove("Keep-Alive");
		out_req
			.headers_mut()
			.insert("Connection", "Upgrade".parse().unwrap());

		if let Ok(value) = host.parse::<HeaderValue>() {
			out_req.headers_mut().insert(header::HOST, value);
		}

		self.merge_cookie_headers(out_req.headers_mut());

		let address = *address;
		return ProxyFuture::Boxed(Box::pin(async move {
			let stream = TcpStream::connect(address).await?;
			let (mut sender, conn) = http1::handshake(TokioIo::new(stream)).await?;

			tokio::spawn(conn.with_upgrades());

			let in_res = sender.send_request(out_req).await?;

			let mut res_out = Response::new(ProxyBody::Empty(Empty::new()));
			res_out.headers_mut().clone_from(in_res.headers());
			*res_out.version_mut() = in_res.version();
			*res_out.status_mut() = in_res.status();

			tokio::spawn(async move {
				let (res_upgraded, req_upgraded) =
					try_join!(hyper::upgrade::on(in_res), hyper::upgrade::on(in_req))?;

				tokio::io::copy_bidirectional(
					&mut TokioIo::new(res_upgraded),
					&mut TokioIo::new(req_upgraded),
				)
				.await
				.map_err(|e| anyhow::Error::from(e))
			});

			Ok(res_out)
		}));
	}
}

impl Service<Request<Incoming>> for Proxy {
	type Response = Response<ProxyBody>;
	type Error = Error;
	type Future = ProxyFuture;

	fn call(&self, req: Request<Incoming>) -> Self::Future {
		if let Some(host) = req
			.uri()
			.authority()
			.map(|authority| authority.as_str())
			.or_else(|| {
				req.headers()
					.get(header::HOST)
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
				return match req
					.headers()
					.get_all(header::UPGRADE)
					.into_iter()
					.all_equal_value()
				{
					Ok(value) if value == "websocket" => {
						self.upgrade(req, host.as_str(), &route.address)
					}
					_ => self.forward(req, host.as_str(), &route.address),
				};
			}
		}

		self.not_found()
	}
}

#[pin_project(project = ProxyBodyProj)]
pub enum ProxyBody {
	Full(#[pin] Full<Bytes>),
	Empty(#[pin] Empty<Bytes>),
	Incoming(#[pin] Incoming),
}

impl Body for ProxyBody {
	type Data = Bytes;
	type Error = Error;

	fn poll_frame(
		self: Pin<&mut Self>,
		cx: &mut Context<'_>,
	) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
		match self.project() {
			ProxyBodyProj::Full(full) => full.poll_frame(cx).map_err(|e| e.into()),
			ProxyBodyProj::Empty(empty) => empty.poll_frame(cx).map_err(|e| e.into()),
			ProxyBodyProj::Incoming(incoming) => incoming.poll_frame(cx).map_err(|e| e.into()),
		}
	}

	fn is_end_stream(&self) -> bool {
		match self {
			ProxyBody::Full(full) => full.is_end_stream(),
			ProxyBody::Empty(empty) => empty.is_end_stream(),
			ProxyBody::Incoming(incoming) => incoming.is_end_stream(),
		}
	}

	fn size_hint(&self) -> SizeHint {
		match self {
			ProxyBody::Full(full) => full.size_hint(),
			ProxyBody::Empty(empty) => empty.size_hint(),
			ProxyBody::Incoming(incoming) => incoming.size_hint(),
		}
	}
}

#[pin_project(project = ProxyFutureProj)]
pub enum ProxyFuture {
	Boxed(#[pin] Pin<Box<dyn Future<Output = Result<Response<ProxyBody>>> + Send + Sync>>),
	Ready(#[pin] Ready<Result<Response<ProxyBody>>>),
}

impl Future for ProxyFuture {
	type Output = Result<Response<ProxyBody>>;

	fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		match self.project() {
			ProxyFutureProj::Boxed(boxed) => boxed.poll(cx),
			ProxyFutureProj::Ready(ready) => ready.poll(cx),
		}
	}
}
