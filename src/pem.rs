use std::{fs, io};

use crate::error::StringError;

pub fn load_certs(filename: &str) -> Result<Vec<rustls::Certificate>, StringError> {
	let file = fs::File::open(filename)
		.map_err(|e| StringError::new(format!("Failed to open cert file {}: {}", filename, e)))?;

	let mut reader = io::BufReader::new(file);

	let certs = rustls_pemfile::certs(&mut reader)
		.map_err(|e| StringError::new(format!("Failed to load cert file: {}", e)))?;

	Ok(certs.into_iter().map(rustls::Certificate).collect())
}

pub fn load_key(filename: &str) -> Result<rustls::PrivateKey, StringError> {
	let file = fs::File::open(filename)
		.map_err(|e| StringError::new(format!("Failed to open key file {}: {}", filename, e)))?;

	let mut reader = io::BufReader::new(file);

	loop {
		let item = rustls_pemfile::read_one(&mut reader).map_err(|e| {
			StringError::new(format!("Failed to read key file {}: {}", filename, e))
		})?;

		match item {
			Some(rustls_pemfile::Item::RSAKey(key)) => return Ok(rustls::PrivateKey(key)),
			Some(rustls_pemfile::Item::PKCS8Key(key)) => return Ok(rustls::PrivateKey(key)),
			Some(rustls_pemfile::Item::ECKey(key)) => return Ok(rustls::PrivateKey(key)),
			Some(_) => {}
			None => {
				return Err(StringError::new(format!(
					"Expected key to exists in key file {}",
					filename
				)))
			}
		};
	}
}
