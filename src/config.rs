use anyhow::{anyhow, Error, Result};
use serde::Deserialize;
use std::{
	env,
	fs::{self, File},
	io::BufReader,
	net::SocketAddr,
};
use tokio_rustls::rustls;
use wildmatch::WildMatch;

#[derive(Debug)]
pub struct StaticConfig {
	pub config_file: String,
	pub http_port: u16,
	pub https_port: u16,
}

impl StaticConfig {
	pub fn load() -> Result<Self> {
		Ok(Self {
			config_file: env::var("CONFIG_FILE").unwrap_or("config.toml".into()),
			http_port: env::var("HTTP_PORT").map_or(Ok(8080), |p| {
				p.parse::<u16>()
					.map_err(|e| anyhow!("HTTP_PORT must be a valid port: {}", e))
			})?,
			https_port: env::var("HTTPS_PORT").map_or(Ok(8443), |p| {
				p.parse::<u16>()
					.map_err(|e| anyhow!("HTTPS_PORT must be a valid port: {}", e))
			})?,
		})
	}
}

#[derive(Debug, Deserialize)]
pub struct DynamicConfig {
	pub cert_file: String,
	pub key_file: String,
	pub routes: Vec<RouteConfig>,
}

impl DynamicConfig {
	pub fn load(static_config: &StaticConfig) -> Result<Self> {
		let content = fs::read_to_string(&static_config.config_file).map_err(|e| {
			anyhow!(
				"Failed to read config file {}: {}",
				static_config.config_file,
				e
			)
		})?;

		return toml::from_str(content.as_str()).map_err(|e| {
			anyhow!(
				"Failed to parse config file {}: {}",
				static_config.config_file,
				e
			)
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
	type Error = Error;

	fn try_from(config: RouteConfigUnchecked) -> Result<Self, Self::Error> {
		Ok(Self {
			host: WildMatch::new(&config.host),
			address: config
				.address
				.parse::<SocketAddr>()
				.map_err(|e| anyhow!("Failed to parse address {}: {}", config.address, e))?,
		})
	}
}

pub struct ServerConfig {
	pub internal: rustls::ServerConfig,
}

impl ServerConfig {
	pub fn load(dynamic_config: &DynamicConfig) -> Result<ServerConfig> {
		let cert_file = File::open(&dynamic_config.cert_file).map_err(|e| {
			anyhow!(
				"Failed to open cert file {}: {}",
				dynamic_config.cert_file,
				e
			)
		})?;
		let key_file = File::open(&dynamic_config.key_file)
			.map_err(|e| anyhow!("Failed to open key file {}: {}", dynamic_config.key_file, e))?;

		let certs = rustls_pemfile::certs(&mut BufReader::new(cert_file))
			.collect::<Result<_, _>>()
			.map_err(|e| {
				anyhow!(
					"Failed to load cert file {}: {}",
					dynamic_config.cert_file,
					e
				)
			})?;
		let key = rustls_pemfile::private_key(&mut BufReader::new(key_file))
			.map_err(|e| anyhow!("Failed to load key file {}: {}", dynamic_config.key_file, e))?
			.ok_or_else(|| anyhow!("Missing key in key file {}", dynamic_config.key_file))?;

		let mut config = rustls::ServerConfig::builder()
			.with_no_client_auth()
			.with_single_cert(certs, key)
			.map_err(|e| anyhow!("Failed to create server config: {}", e))?;

		config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec(), b"http/1.0".to_vec()];

		Ok(Self { internal: config })
	}
}
