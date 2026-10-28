.PHONY: dev-db dev-guacd server agent web build test

dev-db:        ## local Postgres via Homebrew
	brew services start postgresql@16
	psql -d postgres -tc "select 1 from pg_roles where rolname='redesk'" | grep -q 1 || psql -d postgres -c "create role redesk login password 'redesk'"
	psql -d postgres -tc "select 1 from pg_database where datname='redesk'" | grep -q 1 || psql -d postgres -c "create database redesk owner redesk"

dev-guacd:     ## guacd for the RDP/VNC/SSH gateway (Docker)
	docker run -d --name redesk-guacd -p 127.0.0.1:4822:4822 -v $(CURDIR)/server/data/recordings:/recordings guacamole/guacd:1.5.5

server:        ## run the server (serves web/build); gateway on if guacd is running
	cd server && REDESK_ALLOWED_ORIGINS=localhost:5173 REDESK_GUACD_ADDR=127.0.0.1:4822 REDESK_GATEWAY_RECORDING_PATH=/recordings go run ./cmd/redesk-server

web:           ## web dev server with hot reload on :5173
	cd web && bun run dev

agent:         ## run the agent (after `redesk-agent init`)
	cd agent && cargo run --release -- run

build:
	cd web && bun install && bun run build
	cd server && go build -o bin/redesk-server ./cmd/redesk-server
	cd agent && cargo build --release

test:
	cd server && go vet ./... && go test ./...
	cd agent && cargo test
	cd web && bun run check
