# Roadmap
Here's things we would like to work on and add to Dragonfly next.

## Upcoming planned features
* Authentication system
  * Admin login for managing and adopting machines
  * Normal user login - can see machines and adopt new ones, but not reimage or delete any machines
* Configurable front page security
    * Allows open, logged in only, or admin only
    * Allowlist for IP addresses that can access the panel
* Safety mode - "molly guard" - disables power control and reimage controls
* IPMI/BMC/Redfish support
    * Allows for remote power control and monitoring of machines
    * Can be used to power on and adopt a new machine (given a known IPMI address)
    * Can be used to reimage machines by setting PXE mode
    * Power off, reset, power on, power cycle
    * Get power state, machine status, and power draw
* Multi-Factor Authentication

## Low priority planned features
* OpenJBOD support
  * Open source JBOD with a web interface that lets you power cycle disk chassis
* VLAN support
* Bonding/LACP support
* Gamepad support
* Retina/HiDPI display support
* Touchscreen support
* Automatic provisioning of Proxmox clusters
