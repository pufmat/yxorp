mod config;
mod proxy;
mod server;

fn main() {
	if let Err(e) = server::run() {
		eprintln!("{}", e);
	}
}
