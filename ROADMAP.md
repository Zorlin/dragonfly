# Roadmap
Here's things we would like to work on and add to Dragonfly next.

## Upcoming planned features
* Lock individual nodes to prevent them from being reimaged or deleted
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
* A timer that measures how long it takes to deploy each kind of OS Template and even rough heuristics based on hardware (CPU, RAM, etc)
  and uses it to estimate remaining time for longer deployments.
  * This will have a "barber pole/candy spinner" animated progress bar for each deploying node.
  * This will also have a "deployed" status that shows the total number of nodes deployed and the average deployment time.
  * This will be displayed on the main page right after the status counts.
  * Timer exports to Prometheus/Grafana
    * Show all deployment times and stages
    * Show average deployment time by OS template
    * Show average deployment time by hardware type
    * Failed/succeeded counts by OS template, date, and time of day
