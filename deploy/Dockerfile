# Neap backend: indexer + REST/WS API and keeper in one container.
# Build context = repo root.  docker build -f deploy/Dockerfile .
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p neap-indexer -p neap-keeper \
 && cp target/release/neap-indexer target/release/neap-keeper /usr/local/bin/

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates tini \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /usr/local/bin/neap-indexer /usr/local/bin/neap-keeper /usr/local/bin/
COPY deploy/start.sh /usr/local/bin/start.sh
RUN chmod +x /usr/local/bin/start.sh
ENV RUST_LOG=info PORT=10000
EXPOSE 10000
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["/usr/local/bin/start.sh"]
