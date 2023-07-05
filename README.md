# yxorp

A reverse proxy with live reload and TLS support.

## Installation

```bash
cargo install yxorp
```

## Usage

```bash
yxorp
```

## Live reload

```bash
kill -HUP <pid>
```

## Configuration

Environment variables:

* `HTTP_PORT`: Port to bind the HTTP server. Defaults to `8080`.
* `HTTPS_PORT`: Port to bind the HTTPS server. Defaults to `8443`.
* `CONFIG_FILE`: Path to the configuration file. Defaults to `config.toml`.

Configuration file:

```toml
cert_file = "cert.pem"
key_file = "key.pem"

[[routes]]
host = "example.com"
address = "192.168.0.1:80"

[[routes]]
host = "example.net"
address = "192.168.0.2:80"

[[routes]]
host = "*.example.com"
address = "192.168.0.3:80"

[[routes]]
host = "*"
address = "192.168.0.4:80"
```