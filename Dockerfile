FROM rust:1.90-slim-bookworm AS rust-builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
# Cache dependency compilation separately from Paperboy's source.
RUN mkdir rust \
    && printf '%s\n' '#![forbid(unsafe_code)]' > rust/lib.rs \
    && printf '%s\n' 'fn main() {}' > rust/main.rs \
    && printf '%s\n' 'fn main() {}' > rust/tools.rs \
    && printf '%s\n' 'fn main() {}' > rust/updater.rs \
    && cargo build --release --locked \
    && rm -rf rust target/release/.fingerprint/paperboy-* target/release/deps/paperboy* target/release/paperboy*
COPY rust ./rust
RUN cargo build --release --locked \
    && sha256sum Cargo.toml Cargo.lock rust/*.rs > /build/source.sha256

FROM debian:bookworm-slim AS converter
ARG PAPERBOY_VERSION=0.3.0
LABEL org.opencontainers.image.source="https://github.com/thekozugroup/Paperboy" \
    org.opencontainers.image.version=$PAPERBOY_VERSION
RUN apt-get update && apt-get install -y --no-install-recommends \
    libreoffice-writer libreoffice-calc libreoffice-impress poppler-utils \
    fonts-dejavu-core fonts-noto-core \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 1000 paperboy && useradd --uid 1000 --gid paperboy --create-home paperboy \
    && mkdir -p /run/paperboy && chown paperboy:paperboy /run/paperboy
COPY --from=rust-builder /build/target/release/paperboy-tools /usr/local/bin/paperboy-tools
COPY --from=rust-builder /build/source.sha256 /source.sha256
USER paperboy
ENV HOME=/tmp
CMD ["paperboy-tools"]

FROM debian:bookworm-slim AS app
ARG PAPERBOY_VERSION=0.3.0
LABEL org.opencontainers.image.source="https://github.com/thekozugroup/Paperboy" \
    org.opencontainers.image.version=$PAPERBOY_VERSION
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends \
    cups cups-client cups-filters ca-certificates tini \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 1000 paperboy \
    && useradd --uid 1000 --gid paperboy --create-home paperboy \
    && mkdir -p /run/paperboy /updates && chown paperboy:paperboy /run/paperboy /updates \
    && usermod -aG lp,lpadmin paperboy
COPY --from=rust-builder /build/target/release/paperboy /usr/local/bin/paperboy
COPY --from=rust-builder /build/target/release/paperboy-updater /usr/local/bin/paperboy-updater
COPY --from=rust-builder /build/source.sha256 /app/source.sha256
COPY web ./web
COPY docker/cupsd.conf /etc/cups/cupsd.conf
COPY docker/entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh
ENV PAPERBOY_DATA_DIR=/data PAPERBOY_CONVERTER_SOCKET=/run/paperboy/convert.sock \
    CUPS_SERVER=/run/cups/cups.sock
EXPOSE 8025
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s CMD paperboy health
ENTRYPOINT ["/usr/bin/tini", "--", "/entrypoint.sh"]

# Software printer used only by the repeatable Docker integration checks.
FROM app AS printer-qa
RUN apt-get update && apt-get install -y --no-install-recommends avahi-daemon dbus \
    && rm -rf /var/lib/apt/lists/*
HEALTHCHECK NONE
ENTRYPOINT ["/bin/sh", "-c"]
CMD ["mkdir -p /run/dbus /tmp/printed; dbus-daemon --system; avahi-daemon --daemonize; exec ippeveprinter -p 631 -f application/pdf -k -d /tmp/printed 'Paperboy QA'"]

# A plain docker build still produces the production app.
FROM app AS runtime
