use core::task::{Context, Poll};
use futures_util::ready;
use hyper::server::accept::Accept;
use hyper::server::conn::{AddrIncoming, AddrStream};
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_rustls::rustls::ServerConfig;

enum State {
	Handshaking(tokio_rustls::Accept<AddrStream>),
	Streaming(tokio_rustls::server::TlsStream<AddrStream>),
}

pub struct TlsStream {
	state: State,
}

impl TlsStream {
	fn new(stream: AddrStream, config: Arc<RwLock<ServerConfig>>) -> TlsStream {
		let accept = tokio_rustls::TlsAcceptor::from(Arc::new(config.read().unwrap().clone()))
			.accept(stream);
		TlsStream {
			state: State::Handshaking(accept),
		}
	}
}

impl AsyncRead for TlsStream {
	fn poll_read(
		self: Pin<&mut Self>,
		cx: &mut Context,
		buf: &mut ReadBuf,
	) -> Poll<io::Result<()>> {
		let pin = self.get_mut();
		match pin.state {
			State::Handshaking(ref mut accept) => match ready!(Pin::new(accept).poll(cx)) {
				Ok(mut stream) => {
					let result = Pin::new(&mut stream).poll_read(cx, buf);
					pin.state = State::Streaming(stream);
					result
				}
				Err(err) => Poll::Ready(Err(err)),
			},
			State::Streaming(ref mut stream) => Pin::new(stream).poll_read(cx, buf),
		}
	}
}

impl AsyncWrite for TlsStream {
	fn poll_write(
		self: Pin<&mut Self>,
		cx: &mut Context<'_>,
		buf: &[u8],
	) -> Poll<io::Result<usize>> {
		let pin = self.get_mut();
		match pin.state {
			State::Handshaking(ref mut accept) => match ready!(Pin::new(accept).poll(cx)) {
				Ok(mut stream) => {
					let result = Pin::new(&mut stream).poll_write(cx, buf);
					pin.state = State::Streaming(stream);
					result
				}
				Err(err) => Poll::Ready(Err(err)),
			},
			State::Streaming(ref mut stream) => Pin::new(stream).poll_write(cx, buf),
		}
	}

	fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		match self.state {
			State::Handshaking(_) => Poll::Ready(Ok(())),
			State::Streaming(ref mut stream) => Pin::new(stream).poll_flush(cx),
		}
	}

	fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		match self.state {
			State::Handshaking(_) => Poll::Ready(Ok(())),
			State::Streaming(ref mut stream) => Pin::new(stream).poll_shutdown(cx),
		}
	}
}

pub struct TlsAcceptor {
	config: Arc<RwLock<ServerConfig>>,
	incoming: AddrIncoming,
}

impl TlsAcceptor {
	pub fn new(config: Arc<RwLock<ServerConfig>>, incoming: AddrIncoming) -> TlsAcceptor {
		TlsAcceptor { config, incoming }
	}
}

impl Accept for TlsAcceptor {
	type Conn = TlsStream;
	type Error = io::Error;

	fn poll_accept(
		self: Pin<&mut Self>,
		cx: &mut Context<'_>,
	) -> Poll<Option<Result<Self::Conn, Self::Error>>> {
		let pin = self.get_mut();
		match ready!(Pin::new(&mut pin.incoming).poll_accept(cx)) {
			Some(Ok(sock)) => Poll::Ready(Some(Ok(TlsStream::new(sock, pin.config.clone())))),
			Some(Err(e)) => Poll::Ready(Some(Err(e))),
			None => Poll::Ready(None),
		}
	}
}
