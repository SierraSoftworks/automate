# NOTE: This Dockerfile depends on you building the automate binary first.
# It will then package that binary into the image, and use that as the entrypoint.
# This means that running `docker build` is not a repeatable way to build the same
# image, but the benefit is much faster cross-platform builds; a net win.
FROM ubuntu:24.04

LABEL org.opencontainers.image.source=https://github.com/SierraSoftworks/automate-rs
LABEL org.opencontainers.image.description="Automate various aspects of your life with this Rust-based automation system."
LABEL org.opencontainers.image.licenses=MIT

RUN apt-get update && apt-get install -y \
  openssl

ADD ./automate /usr/local/bin/automate

ENTRYPOINT ["/usr/local/bin/automate"]