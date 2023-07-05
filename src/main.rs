mod acceptor;
mod config;
mod error;
mod pem;
mod server;

fn main() {
	if let Err(e) = server::run() {
		eprintln!("{}", e);
	}
}
