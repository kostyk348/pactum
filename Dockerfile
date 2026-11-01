# Pactum Witness: нотариус контента в контейнере
FROM rust:1-slim AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p pactum-cli

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --shell /usr/sbin/nologin witness
COPY --from=build /src/target/release/pactum /usr/local/bin/pactum
USER witness
WORKDIR /data
VOLUME /data
EXPOSE 8090
ENTRYPOINT ["pactum"]
CMD ["witness", "serve", "--dir", "/data", "--listen", "0.0.0.0:8090"]
