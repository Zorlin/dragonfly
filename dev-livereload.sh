#!/bin/bash

# Colors for output
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m' # No Color

# Check if cargo-watch is installed
if ! command -v cargo-watch &> /dev/null
then
    echo -e "${YELLOW}cargo-watch not found. Attempting to install...${NC}"
    cargo install cargo-watch
    if [ $? -ne 0 ]; then
        echo -e "${YELLOW}Failed to install cargo-watch. Please install it manually:${NC}"
        echo -e "${CYAN}cargo install cargo-watch${NC}"
        exit 1
    fi
fi

echo -e "${GREEN}Starting Dragonfly in development mode with live reloading...${NC}"
echo -e "${YELLOW}Open http://localhost:3000 in your browser${NC}"
echo -e "${YELLOW}Server will restart and browser will reload on code changes.${NC}"

# Run with cargo-watch to automatically rebuild and restart the server on Rust code changes.
# Template changes are handled internally by the server for true hot-reloading.
# The DRAGONFLY_DEV_MODE env var enables the tower-livereload middleware and internal template watching.
DRAGONFLY_DEV_MODE=1 cargo watch -q -c -w src -w crates/dragonfly-server/src -w crates/dragonfly-common/src -e rs -x 'command cargo run -- --' -- "$@" 