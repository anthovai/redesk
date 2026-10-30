.PHONY: dev-db dev-guacd server agent web build test

dev-db:        ## local Postgres via Homebrew
	brew services start postgresql@16
	psql -d postgres -tc "select 1 from pg_roles where rolname='aegiron'" | grep -q 1 || psql -d postgres -c "create role aegiron login password 'aegiron'"
	psql -d postgres -tc "select 1 from pg_database where datname='aegiron'" | grep -q 1 || psql -d postgres -c "create database aegiron owner aegiron"

dev-guacd:     ## guacd for the RDP/VNC/SSH gateway (Docker)
	mkdir -p server/data/recordings
	-docker run --rm --user root --entrypoint chown -v $(CURDIR)/server/data/recordings:/recordings guacamole/guacd:1.5.5 guacd:guacd /recordings
	docker run -d --name aegiron-guacd -p 127.0.0.1:4822:4822 -v $(CURDIR)/server/data/recordings:/recordings guacamole/guacd:1.5.5

server:        ## run the server (serves web/build); gateway on if guacd is running
	cd server && AEGIRON_ALLOWED_ORIGINS=localhost:5173 AEGIRON_GUACD_ADDR=127.0.0.1:4822 AEGIRON_GATEWAY_RECORDING_PATH=/recordings go run ./cmd/aegiron-server

web:           ## web dev server with hot reload on :5173
	cd web && bun run dev

agent:         ## run the agent (after `aegiron-agent init`)
	cd agent && cargo run --release -- run

build:
	cd web && bun install && bun run build
	cd server && go build -o bin/aegiron-server ./cmd/aegiron-server
	cd agent && cargo build --release

test:
	cd server && go vet ./... && go test ./...
	cd agent && cargo test
	cd web && bun run check
