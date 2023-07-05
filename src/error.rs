use core::fmt;
use std::error::Error;

#[derive(Debug)]
pub struct StringError {
	details: String,
}

impl StringError {
	pub fn new<T: Into<String>>(details: T) -> Self {
		Self {
			details: details.into(),
		}
	}
}

impl fmt::Display for StringError {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}", self.details)
	}
}

impl Error for StringError {
	fn description(&self) -> &str {
		&self.details
	}
}
