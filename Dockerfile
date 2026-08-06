# mdcast-server container image.
#
# Runtime binaries the render engines shell out to:
#   - pandoc: docx/odt/pptx/html-reveal targets (subprocess, apt package)
#   - typst:  MDCAST_TYPST=cli mode compiles PDF/HTML in a killable child
#     process with timeout + memory cap (issue #73); pinned to the same 0.14
#     line as the typst crates, musl build so it runs on slim debian as-is.
#     `library` mode never invokes it — shipping it just keeps both modes
#     usable from one image.

FROM rust:1.97-bookworm AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY mdcast-api/ mdcast-api/
COPY mdcast-client/ mdcast-client/
COPY src/ src/
COPY embedded/ embedded/
RUN cargo build --release --features server --bin mdcast-server

FROM debian:bookworm-slim
ARG TYPST_VERSION=v0.14.2
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl pandoc xz-utils \
    && curl -fsSL "https://github.com/typst/typst/releases/download/${TYPST_VERSION}/typst-x86_64-unknown-linux-musl.tar.xz" \
       | tar -xJ --strip-components=1 -C /usr/local/bin typst-x86_64-unknown-linux-musl/typst \
    && apt-get purge -y curl xz-utils && apt-get autoremove -y \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=build /app/target/release/mdcast-server ./
# Loopback is useless inside a container; everything else keeps the
# documented MDCAST_* defaults.
ENV MDCAST_BIND=0.0.0.0:8080
EXPOSE 8080
ENTRYPOINT ["./mdcast-server"]
