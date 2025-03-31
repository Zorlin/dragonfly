# Hot Reloading for Dragonfly

To enable hot reloading during development, run the development script:

```bash
./dev.sh
```

## How It Works

This setup uses two components for a seamless development experience:

1.  **`cargo-watch`**: This tool monitors your source files (Rust code, templates, static assets) within the `crates/` directory. When it detects a change, it automatically recompiles and restarts the Dragonfly server.
2.  **`tower-livereload`**: This Axum middleware injects a small script into your HTML pages during development (when `DRAGONFLY_DEV_MODE=1` is set). This script listens for the server to restart. When the server restarts (triggered by `cargo-watch`), the script automatically reloads the browser page.

This combination ensures that both server-side changes (Rust code, compiled templates) and client-side changes trigger an automatic browser refresh.

## Prerequisites

- **`cargo-watch`**: The `dev.sh` script will attempt to install it automatically if it's not found. You can also install it manually:
  ```bash
  cargo install cargo-watch
  ```

## Development Workflow

1.  Run `./dev.sh` from the project root.
2.  Open `http://localhost:3000` in your browser.
3.  Make changes to any `.rs`, `.html`, `.js`, or `.css` file within the `crates/` directory.
4.  `cargo-watch` will detect the change, rebuild/restart the server.
5.  `tower-livereload` will detect the server restart and automatically refresh your browser page.

This provides a smooth workflow where your code changes are reflected almost instantly in the browser.
