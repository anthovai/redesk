# Build context: repository root (see docker-compose.yml)
FROM oven/bun:1 AS web
WORKDIR /src/web
COPY web/package.json web/bun.lock ./
RUN bun install --frozen-lockfile
COPY web/ ./
RUN bun run build

FROM golang:1.27 AS server
WORKDIR /src/server
COPY server/go.mod server/go.sum ./
RUN go mod download
COPY server/ ./
RUN CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /redesk-server ./cmd/redesk-server

FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=server /redesk-server /redesk-server
COPY --from=web /src/web/build /web
ENV REDESK_WEB_DIR=/web REDESK_DATA_DIR=/data
VOLUME /data
EXPOSE 21114
ENTRYPOINT ["/redesk-server"]
