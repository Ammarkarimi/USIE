# USIE image: the API, the worker and the analysis engine in one image.
# Build from the repository root:
#   docker build -f infrastructure/docker/Dockerfile -t usie .

FROM rust:1.90-bookworm AS engine
WORKDIR /src/engines
COPY engines/ ./
RUN cargo build --release --locked -p usie-engine

FROM node:22-bookworm-slim AS web
WORKDIR /src
RUN corepack enable
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml tsconfig.base.json ./
COPY packages/ packages/
COPY apps/web/ apps/web/
RUN pnpm install --frozen-lockfile && pnpm --filter @usie/web build

FROM golang:1.24-bookworm AS services
WORKDIR /src
ENV CGO_ENABLED=0 GOTOOLCHAIN=local
COPY go.mod go.sum ./
RUN go mod download
COPY database/ database/
COPY internal/ internal/
COPY services/ services/
ARG VERSION=dev
RUN go build -trimpath -ldflags "-s -w -X github.com/ammarkarimi/usie/internal/platform/runtime.Version=${VERSION}" \
      -o /out/ ./services/api-gateway/cmd/usie-api ./services/execution-orchestrator/cmd/usie-worker

FROM debian:bookworm-slim
# git is needed by the worker to clone repositories; nothing else is installed.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates git \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home-dir /var/lib/usie --create-home usie
COPY --from=engine /src/engines/target/release/usie-engine /usr/local/bin/usie-engine
COPY --from=services /out/usie-api /out/usie-worker /usr/local/bin/
COPY --from=web /src/apps/web/dist /usr/share/usie/web
ENV USIE_ENGINE_BIN=/usr/local/bin/usie-engine \
    USIE_WEB_DIR=/usr/share/usie/web \
    USIE_DATA_DIR=/var/lib/usie \
    USIE_HTTP_ADDR=:8080
USER usie
WORKDIR /var/lib/usie
EXPOSE 8080
CMD ["usie-api"]
