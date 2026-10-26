# Static musl binary on an empty base image. Build: podman build -t paperclipped-nextcloud .
FROM docker.io/library/rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p paperclipped-nextcloud \
 && strip target/release/paperclipped-nextcloud

FROM scratch
LABEL org.opencontainers.image.source="https://github.com/deltadrai/paperclipped-nextcloud" \
      org.opencontainers.image.description="Provision mail + Nextcloud identities for Paperclip agents" \
      org.opencontainers.image.licenses="MIT"
COPY --from=build /src/target/release/paperclipped-nextcloud /usr/local/bin/paperclipped-nextcloud
ENV PCNC_CONFIG=/etc/paperclipped-nextcloud/config.yaml
# Runs as root inside the container: under rootless podman that is the host user owning the
# docker-mailserver volumes this server edits.
ENTRYPOINT ["/usr/local/bin/paperclipped-nextcloud"]
CMD ["serve"]
