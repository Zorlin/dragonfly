# 🐉 Dragonfly

NOTE: Dragonfly is still in development and not ready for production use.

This README reflects the goals of the project for an initial release, ***and is not yet reality***.

> 🧠 The **Bare Metal Infrastructure Management System** that makes metal effortless —
> built in Rust, styled in Tailwind, designed for efficiency and reliability.

Dragonfly is a **fast**, **flexible**, and ***satisfying*** platform
for managing and deploying bare-metal infrastructure at any scale.

Whether you’ve got 5 test VMs or 5,000 enterprise grade machines in a datacenter...

Dragonfly will help.

---

## What does it do?
Dragonfly is a virtual and bare-metal provisioning and orchestration system.
It answers the question:

> “I just racked a machine—what happens next?”

When a machine boots via PXE, it loads a minimal Alpine-based agent that registers itself with the Dragonfly server.
From there, Dragonfly can:

* Grab details about the machine

* Automatically or manually assign an operating system and optional role

* Install the operating system

Dragonfly turns unconfigured hardware into usable infrastructure —
automatically, securely, and *quickly*.

## ✨ Features
The main highlights:
- 🌍 Web interface for managing, deploying
  and monitoring your machines and infrastructure.
- 📡 Automatic machine registration via PXE + Dragonfly Agent
- 🔄 Automated OS installation with support for ISOs, PXE, and chainloading.
- 🧚 Powered by Tinkerbell under the hood
  for wide compatibility and support for just about any hardware.
- 🏎️ Deployment as fast as four minutes.
- 🛰️ Distributed storage and IPFS deployment
  for integrated data management.

More features:
- 🔒 Login system with admin/user roles and permissions
- 🔧 Reimage any machine in two clicks
- 🧸 **Safety Mode (Molly Guard)** — avoid accidentally nuking a machine
- 🚀 Built-in IPMI/BMC/Redfish power control
  and SSH control support for flexible node power operations.
- 🧠 Effortless grouping and tagging for your machines,
  and emoji/font-awesome icon support for easy visual identification.
- 💈 Real-time deployment tracking with progress bars and status indicators.
- 🖼️ Ready for Retina, ultrawide and kiosk displays
- 🏷️ "Just Type" experience — with bulk editing, drag-fill, and autocomplete  
- 🎨 Tailwind-powered theming — pick your aesthetic or import your own.
- 🩻 Introspection - view details of your machines,
  including hardware, OS, and network configuration.
- 🔍 Search - find any machine by name, tag, or ID.
- 📊 Granular reporting and monitoring of your machines.
- 📦 Built in image management for OS and drivers.
- 🎮 Gamepad and touchscreeen support for easy navigation of the UI.

## 🛣️ Roadmap

See [ROADMAP.md](ROADMAP.md) for upcoming features and planned work.

## 🚀 Running Dragonfly

You'll need Rust installed to use Dragonfly. Later in development, we'll be providing pre-built binaries and Docker images.

To get a binary to run:
```bash
cargo build --release -p dragonfly-server
```

For development:
```bash
cargo run -p dragonfly-server
```

Once you've got Dragonfly up and running, you can access the web interface at [http://localhost:9800](http://localhost:9800).

## 🗄️ Database Integration

Dragonfly uses the SQLx crate for database integration.

## 📚 Credits

Dragonfly is inspired by and intended as a GUI for the Tinkerbell project. It would not be possible without their work, and we're grateful for their efforts.

We also thank other projects that Dragonfly builds on, such as:
* [MooseFS](https://moosefs.org/)
* [CubeFS](https://cubefs.io/)
* [Tinkerbell](https://tinkerbell.org/)
* [Alpine Linux](https://alpinelinux.org/)
* [k0s](https://k0s.sh/)
* [Proxmox](https://proxmox.com/)
* [OpenJBOD](https://github.com/OpenJBOD)

Thanks to [Taylor Vick](https://unsplash.com/photos/cable-network-M5tzZtFCOfs) for the login page background image ("racks.jpg")

## 📝 License

Dragonfly is licensed under the AGPLv3 license.

See the [LICENSE](LICENSE) for more details.