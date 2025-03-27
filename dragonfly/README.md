# Dragonfly

Dragonfly is a Rust web application that enables simple management of bare metal datacenter infrastructure, providing both an API for machine registration and a UI for OS installation control.

## Features

- Machine registration API for PXE-booted machines
- Web UI for managing OS installations per machine
- Integration with Tinkerbell.org infrastructure
- Simple, secure authentication system
- Real-time machine status monitoring
- SQLite for lightweight, embedded storage

## User stories

### User story 1: Register a machine

As a user, I want to register a machine so that I can manage it.

I boot the machine, and select the first PXE option that comes up.

The machine boots an Alpine Linux minimal image with the Dragonfly agent embedded in it,
then connects to the local Dragonfly instance.

The agent reports the machine's hardware details to the server, and the server assigns an OS to the machine.

The agent, knowing it's booted into the Dragonfly image, will then chainload IPXE and load the standard Tinkerbell.org boot process.

```
kexec -l /usr/share/ipxe/ipxe.lkrn --command-line="dhcp && chain http://10.7.1.30:8080/hookos.ipxe"
```

The hookos.ipxe script will then download the OS image and install it to the machine.

## Technical Stack

- **Backend**: Axum (async web framework)
- **Database**: SQLite (embedded SQL database)
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
  db.rs            # SQLite integration
/templates
  index.html       # Main dashboard
  machine_list.html # Machine management view
/static
  style.css        # Styling
```

## Configuration

Dragonfly uses SQLite for data storage, which provides a lightweight, embedded database solution. By default, it stores data in a local file named `sqlite.db`.

Configuration options include:
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

## Database Integration

Dragonfly uses SQLite, an embedded database engine. Key benefits include:

- **Simplicity**: Familiar SQL syntax
- **Zero Configuration**: No separate server process required
- **Reliability**: ACID-compliant transactions
- **Performance**: Efficient for moderate workloads
- **Lightweight**: Minimal resource requirements

The project uses:
- Axum for the web framework
- SQLite for data storage
- Askama for HTML templating
- Serde for JSON serialization
- Kubernetes integration for Tinkerbell.org compatibility

## Credits

Dragonfly is inspired by and intended as a GUI for the Tinkerbell project.

## License

Dragonfly is licensed under the AGPLv3 license.
