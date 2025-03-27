# Dragonfly

Dragonfly is a Rust web application that enables simple management of bare metal datacenter infrastructure, providing both an API for machine registration and a UI for OS installation control.

## Features

- Machine registration API for PXE-booted machines
- Web UI for managing OS installations per machine
- Integration with Tinkerbell.org infrastructure
- Simple, secure authentication system
- Real-time machine status monitoring
- rqlite for durable, distributed SQLite storage

## Technical Stack

- **Backend**: Axum (async web framework)
- **Database**: rqlite (distributed SQLite database)
- **Frontend**: Askama (compile-time templating)
- **Data Format**: JSON for API communication
- **Optional Frontend Enhancements**: 
  - htmx for dynamic interactions
  - Alpine.js for UI components

## Project Structure

```
/src
  main.rs          # Application entry point
  api.rs           # JSON API endpoints
  ui.rs            # HTML UI routes
  models.rs        # Shared data structures
  db.rs            # rqlite integration
/templates
  index.html       # Main dashboard
  machine_list.html # Machine management view
/static
  style.css        # Styling
```

## Configuration

Dragonfly uses rqlite for data storage, which provides distributed SQLite capabilities. By default, it connects to a rqlite instance at localhost:4001. You can configure the rqlite host using the environment variable:

```bash
export RQLITE_HOST="your-rqlite-host:4001"
```

Other configuration options include:
- Authentication settings
- API endpoints
- UI customization
- Server port and binding address via command-line options:
  ```
  dragonfly --port 3000 --host 0.0.0.0
  ```

## Permissions and RBAC

Dragonfly requires a Kubernetes ServiceAccount with permissions to:
- Read, write, and list `apiVersion: tinkerbell.org/v1alpha1/Hardware`
- Read, write, and list `apiVersion: tinkerbell.org/v1alpha1/Template`
- Read, write, and list `apiVersion: tinkerbell.org/v1alpha1/Workflow`

Alternatively, you can provide a kubeconfig file with equivalent permissions.

## Usage

### API Endpoints

The API provides endpoints for:
- Machine registration
- Status reporting
- OS installation control
- Hardware information management

### Web Interface

The web UI allows operators to:
- View all registered machines
- Assign OS installations
- Monitor machine status
- Manage hardware configurations

### Machine Agent

Machines are expected to run a small agent upon PXE booting that:
1. Connects to the local Dragonfly instance
2. Reports hardware details
3. Receives installation instructions
4. Manages the boot process

## Building

Dragonfly is built using Cargo:

```bash
cargo build
```

For development:
```bash
cargo run
```

## rqlite Integration

Dragonfly uses rqlite, a distributed database built on SQLite. Key benefits include:

- **Simplicity**: Familiar SQL syntax and SQLite compatibility
- **Distributed**: Built-in consensus and cluster management
- **Reliability**: Robust replication via the Raft consensus algorithm
- **Performance**: High-speed reads and consistent writes
- **Lightweight**: Minimal resource requirements

The database schema creates a straightforward `machines` table for tracking machine registration, OS assignments, and status updates. The implementation includes an in-memory fallback mechanism for resilience if the database connection fails.

## Setting Up rqlite

To run rqlite locally for development:

```bash
# Download and start rqlite
wget https://github.com/rqlite/rqlite/releases/download/v7.19.0/rqlite-v7.19.0-linux-amd64.tar.gz
tar xvfz rqlite-v7.19.0-linux-amd64.tar.gz
cd rqlite-v7.19.0-linux-amd64
./rqlited -node-id 1 ~/node.1
```

For production deployments, consider using a multi-node cluster for high availability.

## Development

The project uses:
- Axum for the web framework
- rqlite for data storage
- Askama for HTML templating
- Serde for JSON serialization
- Kubernetes integration for Tinkerbell.org compatibility

## Credits

Dragonfly is inspired by and intended as a GUI for the Tinkerbell project.

## License

Dragonfly is licensed under the AGPLv3 license.
