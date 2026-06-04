FROM rust:1.88-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 1000 opensnow \
    && useradd --uid 1000 --gid 1000 --home-dir /home/opensnow --create-home --shell /usr/sbin/nologin opensnow \
    && mkdir -p /home/opensnow/.opensnow /data/opensnow \
    && chown -R opensnow:opensnow /home/opensnow /data/opensnow
COPY --from=builder /app/target/release/opensnow /usr/local/bin/opensnow
COPY --from=builder /app/target/release/opensnow-mcp /usr/local/bin/opensnow-mcp
ENV HOME=/home/opensnow
WORKDIR /home/opensnow
EXPOSE 8080 5433 8090
USER opensnow
ENTRYPOINT ["opensnow"]
CMD ["start"]
