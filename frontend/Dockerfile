# Astro SSR portal. Build with:
#   docker buildx build --platform linux/amd64 \
#     --build-arg PORTAL_HOSTS=portal.dev.example,portal.example -t a2a-portal frontend/
# PORTAL_HOSTS is baked in at build time (Astro `security.allowedDomains`).
# List every environment's hostname so one digest can be promoted unchanged.

ARG NODE_IMAGE=node:24.21.0-trixie-slim@sha256:173f125896c3b47ddf056734c7ea789d04595a6a08769a8f78e0df642781fb66
ARG RUNTIME_IMAGE=gcr.io/distroless/nodejs24-debian13:nonroot@sha256:6abfcb367ebc291572c774c1592a986be2ab26a2486f73093401327495a5962a

FROM ${NODE_IMAGE} AS build
WORKDIR /app
COPY package.json package-lock.json ./
RUN --mount=type=cache,target=/root/.npm npm ci --no-audit --no-fund
COPY . .
ARG PORTAL_HOSTS=127.0.0.1,localhost
ENV PORTAL_HOSTS=${PORTAL_HOSTS}
RUN npm run build

FROM ${NODE_IMAGE} AS deps
WORKDIR /app
COPY package.json package-lock.json ./
RUN --mount=type=cache,target=/root/.npm npm ci --omit=dev --ignore-scripts --no-audit --no-fund

FROM ${RUNTIME_IMAGE} AS portal
WORKDIR /app
COPY --from=deps /app/node_modules ./node_modules
COPY --from=build /app/dist ./dist
COPY package.json ./
USER 65532:65532
ENV NODE_ENV=production HOST=0.0.0.0 PORT=4321
EXPOSE 4321
# The distroless entrypoint is the node binary.
CMD ["dist/server/entry.mjs"]
