# Automate

Automate is a Rust-based automation server designed to automate
common manual tasks and use Todoist to request human involvement
when necessary.

It facilitates things like calendar sync, RSS syndication, and
the automatic management of GitHub notifications; as well as
handling webhooks from services like Tailscale and Honeycomb.

## Configuration

Automate is configured via a `config.toml` file. An example
configuration file can be found at `config.example.toml`. You can copy
this file to `config.toml` and modify it to suit your needs.

## Running

To run Automate, ensure you have Rust installed and then execute:

```bash
cargo run --release
```
