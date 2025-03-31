# Roadmap
Here's things we would like to work on and add to Dragonfly next.

This is speculative and subject to change.

## Upcoming planned features
* First run wizard:

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

Simple mode.
Tinkerbell mode.
Distributed mode.

Simple mode will use *direct PXE imaging completely agentlessly*, simply using MAC-to-hostname mapping (with the same reverse DNS lookup trick where a machine that attempts to PXE boot from it will be looked up in reverse DNS, so if it has a static DHCP lease and a DNS name, it can not just be assigned a real hostname *but also tags and roles* automatically. It'll literally just Kickstart/preseed/whatever VMs instead of using our deployment system, and it'll be slower but the tradeoff is that it will directly install machines without any intermediate steps.

Distributed mode will stretch and loadbalance the IPXE distribution/image distribution system, as well as make the entire system effortlessly HA. And there will be a "Convert to Dragonfly" button on newly deployed machines that turns a machine into a Dragonfly node automatically and joins it to the existing cluster.

If the user runs:
`dragonfly`

And no install, no run, no flags:
You launch the Dragonfly Demo Experience™

What This Demo Mode Should Do:
✅ Run in-memory only
No filesystem writes

No k3s startup

No agent listening

PXE disabled

Temporary port binding (e.g., localhost:3000)

Just enough to render the full Web UI with mock data

🧑‍🏫 Show the Real UI
Simulated machine list

Realtime-looking status

Tag editing

Tinkerbell workflows “in progress”

But everything is ephemeral and safe to explore

🧭 Show a banner:
Demo Mode: Dragonfly is not installed yet.
This is a preview — none of your hardware is touched.
[🚀 Install Now] [📖 Docs] [🛠 Advanced Setup]

🧠 Why This Is Brilliant
🪶 Zero commitment

⚡ Immediate UX payoff

🧠 Helps people decide without docs or flags

📦 Makes dragonfly self-explanatory — the binary is the experience

🧩 Bonus
Let users type:

dragonfly --demo

to re-enter it later — great for testing or CI screenshots.
