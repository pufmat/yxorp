use crate::{error::StringError, pem};

use rustls::ServerConfig;
use serde::Deserialize;
use std::{env, fs, net::SocketAddr};
use wildmatch::WildMatch;

#[derive(Debug)]
pub struct StaticConfig {
	pub config_file: String,
	pub http_port: u16,
	pub https_port: u16,
}

impl StaticConfig {
	pub fn load() -> Result<Self, StringError> {
		return Ok(Self {
			config_file: env::var("CONFIG_FILE").unwrap_or("config.toml".into()),
			http_port: env::var("HTTP_PORT").map_or(Ok(8080), |p| {
				p.parse::<u16>()
					.map_err(|e| StringError::new(format!("HTTP_PORT must be a valid port: {}", e)))
			})?,
			https_port: env::var("HTTPS_PORT").map_or(Ok(8443), |p| {
				p.parse::<u16>().map_err(|e| {
					StringError::new(format!("HTTPS_PORT must be a valid port: {}", e))
				})
			})?,
		});
	}
}

#[derive(Debug, Deserialize)]
pub struct DynamicConfig {
	pub cert_file: String,
	pub key_file: String,
	pub routes: Vec<RouteConfig>,
}

impl DynamicConfig {
	pub fn load(static_config: &StaticConfig) -> Result<Self, StringError> {
		let content = fs::read_to_string(&static_config.config_file).map_err(|e| {
			StringError::new(format!(
				"Failed to read config file {}: {}",
				static_config.config_file, e
			))
		})?;

		return toml::from_str(content.as_str()).map_err(|e| {
			StringError::new(format!(
				"Failed to parse config file {}: {}",
				static_config.config_file, e
			))
		});
	}
}

#[derive(Debug, Deserialize)]
#[serde(try_from = "RouteConfigUnchecked")]
pub struct RouteConfig {
	pub host: WildMatch,
	pub address: SocketAddr,
}

#[derive(Debug, Deserialize)]
struct RouteConfigUnchecked {
	host: String,
	address: String,
}

impl TryFrom<RouteConfigUnchecked> for RouteConfig {
	type Error = StringError;

	fn try_from(config: RouteConfigUnchecked) -> Result<Self, Self::Error> {
		Ok(Self {
			host: WildMatch::new(&config.host),
			address: config.address.parse::<SocketAddr>().map_err(|e| {
				StringError::new(format!("Failed to parse address {}: {}", config.address, e))
			})?,
		})
	}
}

pub fn load_server_config(dynamic_config: &DynamicConfig) -> Result<ServerConfig, StringError> {
	let certs = pem::load_certs(dynamic_config.cert_file.as_str())?;
	let key = pem::load_key(dynamic_config.key_file.as_str())?;

	let mut server_config = rustls::ServerConfig::builder()
		.with_safe_defaults()
		.with_no_client_auth()
		.with_single_cert(certs, key)
		.map_err(|e| StringError::new(format!("Failed to create server config: {}", e)))?;

	server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec(), b"http/1.0".to_vec()];

	return Ok(server_config);
}
