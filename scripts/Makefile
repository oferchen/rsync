# Makefile
# Simple targets to wire the scripts

.PHONY: interop server client coverage coverage-open coverage-html coverage-lcov

interop:
	./scripts/rsync-interop-orchestrator.sh

server:
	./scripts/rsync-interop-server.sh

client:
	./scripts/rsync-interop-client.sh

# Coverage targets
coverage:
	./scripts/coverage.sh

coverage-open:
	./scripts/coverage.sh --open

coverage-html:
	./scripts/coverage.sh --html-only

coverage-lcov:
	./scripts/coverage.sh --lcov-only

