# Runtime images for the pilot workloads. Build one with:
#   docker buildx build --platform linux/amd64 --target api -t a2a-api backend/
# Targets: api, worker-agent, reviewer-agent. Base images are pinned by digest;
# the builder and runtime share Debian 13 so the glibc ABI matches.

ARG RUST_IMAGE=rust:1.100.0-slim-trixie@sha256:6f14b2b4b278ec0d50aa1d69541c304ce0f95e0f6268487aa9924af1c2e9aebe
ARG RUNTIME_IMAGE=gcr.io/distroless/cc-debian13:nonroot@sha256:cb87acc574abe719b6e5c547384defa2f7b1f24af9605d3f618cf852495d8be2

FROM ${RUST_IMAGE} AS build
WORKDIR /src
COPY . .
# Cache mounts keep the registry and incremental target dir between builds;
# binaries are copied out because the target dir is not part of the layer.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    cargo build --release --locked -p api -p worker-agent -p reviewer-agent \
 && mkdir /out \
 && cp target/release/api target/release/worker-agent target/release/reviewer-agent /out/

FROM ${RUNTIME_IMAGE} AS runtime
# distroless :nonroot runs as uid/gid 65532 and has no shell or package manager.
USER 65532:65532
ENV LOG_FORMAT=json RUST_LOG=info

FROM runtime AS api
COPY --from=build /out/api /usr/local/bin/api
# Container-friendly default; the Kubernetes Service is the only way in.
ENV ORCHESTRATOR_BIND=0.0.0.0:8080
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/api"]
CMD ["serve"]

FROM runtime AS worker-agent
COPY --from=build /out/worker-agent /usr/local/bin/worker-agent
ENV WORKER_BIND=0.0.0.0:9101
EXPOSE 9101
ENTRYPOINT ["/usr/local/bin/worker-agent"]

FROM runtime AS reviewer-agent
COPY --from=build /out/reviewer-agent /usr/local/bin/reviewer-agent
ENV REVIEWER_BIND=0.0.0.0:9102
EXPOSE 9102
ENTRYPOINT ["/usr/local/bin/reviewer-agent"]
