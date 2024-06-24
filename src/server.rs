use crate::{
	config::{DynamicConfig, ServerConfig, StaticConfig},
	proxy::Proxy,
};

use anyhow::{anyhow, Result};
use hyper_util::{
	rt::{TokioExecutor, TokioIo},
	server::conn::auto,
};
use std::{
	net::{Ipv4Addr, SocketAddrV4},
	sync::{Arc, RwLock},
};
use tokio::net::TcpListener;

#[cfg(unix)]
use tokio::signal::unix::SignalKind;

#[tokio::main]
pub async fn run() -> Result<()> {
	let static_config = StaticConfig::load()?;

	let dynamic_config = Arc::new(RwLock::new(DynamicConfig::load(&static_config)?));

	let server_config = Arc::new(RwLock::new(ServerConfig::load(
		&dynamic_config.read().unwrap(),
	)?));

	let http_addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, static_config.http_port);
	let https_addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, static_config.https_port);

	let http_listener = TcpListener::bind(http_addr)
		.await
		.map_err(|e| anyhow!("Failed to bind address {}: {}", http_addr, e))?;
	let https_listener = TcpListener::bind(https_addr)
		.await
		.map_err(|e| anyhow!("Failed to bind address {}: {}", https_addr, e))?;

	tokio::spawn(serve_unsecure(http_listener, dynamic_config.clone()));

	tokio::spawn(serve_secure(
		https_listener,
		dynamic_config.clone(),
		server_config.clone(),
	));

	println!("Server started");

	#[cfg(unix)]
	tokio::spawn(handle_reload(static_config, dynamic_config, server_config));

	tokio::signal::ctrl_c().await.unwrap();

	println!("Server stopped");

	Ok(())
}

async fn serve_unsecure(listener: TcpListener, dynamic_config: Arc<RwLock<DynamicConfig>>) {
	loop {
		if let Ok((stream, _)) = listener.accept().await {
			let dynamic_config = dynamic_config.clone();

			tokio::spawn(async move {
				let io = TokioIo::new(stream);

				auto::Builder::new(TokioExecutor::new())
					.serve_connection_with_upgrades(io, Proxy::new_unsecure(dynamic_config))
					.await
			});
		}
	}
}

async fn serve_secure(
	listener: TcpListener,
	dynamic_config: Arc<RwLock<DynamicConfig>>,
	server_config: Arc<RwLock<ServerConfig>>,
) {
	loop {
		if let Ok((stream, _)) = listener.accept().await {
			let dynamic_config = dynamic_config.clone();
			let server_config = Arc::new(server_config.read().unwrap().internal.clone());

			tokio::spawn(async move {
				let tls_acceptor = tokio_rustls::TlsAcceptor::from(server_config);

				let tls_stream = tls_acceptor.accept(stream).await?;
				let io = TokioIo::new(tls_stream);

				auto::Builder::new(TokioExecutor::new())
					.serve_connection_with_upgrades(io, Proxy::new_secure(dynamic_config))
					.await
			});
		}
	}
}

#[cfg(unix)]
async fn handle_reload(
	static_config: StaticConfig,
	dynamic_config: Arc<RwLock<DynamicConfig>>,
	server_config: Arc<RwLock<ServerConfig>>,
) {
	let mut hup_signal = tokio::signal::unix::signal(SignalKind::hangup()).unwrap();

	loop {
		hup_signal.recv().await;

		match || -> Result<()> {
			let mut dynamic_config = dynamic_config.write().unwrap();
			*dynamic_config = DynamicConfig::load(&static_config)?;
			let mut server_config = server_config.write().unwrap();
			*server_config = ServerConfig::load(&dynamic_config)?;
			Ok(())
		}() {
			Ok(()) => println!("Config reloaded successfully"),
			Err(e) => println!("Failed to reload config: {}", e),
		}
	}
}
