# User stories

## Dragonfly Server Deployment

### Overview

This document outlines the core user flows and behaviors for Dragonfly across various scenarios. It defines how Dragonfly behaves when executed from the CLI or server context, what installation looks like, and how the onboarding experience adapts based on state. It also describes the different operating modes available to the user post-install.

### Legend
🚧 In progress
🚀 Completed
✅ Completed, needs unit tests
---

### 🧪 Scenario A: CLI Invocation (e.g., `dragonfly`)

#### Default behavior:
When a user runs `dragonfly` from the command line:

#### If Dragonfly is already installed:
🚀 Completed
- Ensure all Dragonfly services are running.
- Print the local address of the WebUI (e.g., `http://localhost:3000`).
- Print the full CLI help text.

#### If Dragonfly is not installed:
🚀 Completed
- Print the CLI help text.
- Do **not** attempt to inspect any server state.
- Emphasize that `dragonfly install` is the next step.

---

### 🖥️ Scenario B: Server Mode (`dragonfly server`)

#### If Dragonfly is already installed:

##### If a mode has been selected previously:
🏗️ To do
- Boot straight into the active mode (Simple, Flight, or Swarm).
- If login enforcement is enabled, present the login screen.

#### If no mode has been selected yet:
🏗️ To do
- Display the `welcome.html` onboarding screen.

The Welcome screen intentionally uses the chrome from the main Dragonfly UI, but removes the Dashboard, Machines, Settings, and Login sections until the user has chosen a mode.

Once the user has chosen a mode (they will be encouraged to choose Flight mode by default), we show the normal UI and fade in the normal buttons.

They are prompted to change the initial password, and are given the option to change it later. It saves the initial password as a secret in Kubernetes in case the user forgets the initial password, but *will not* store any passwords the user changes it to later.

The user loads into the main interface, and having selected Flight mode, Dragonfly will start downloading HookOS images.

When the user selects an OS, a pullthrough cache is used to store the OS images as they are imaged onto the machine.

The user can select a machine from the machine list and change its OS to a different OS and redeploy it, or change its hostname, friendly name, MAC address, or tags.

#### If Dragonfly is **not** installed:
🚧 In progress
- Boot into a full **Demo Experience**:
  - Fake data, in-memory only.
  - UI fully interactive.
  - 🚧 Banner: *"Demo Mode: Dragonfly is not installed. This is a preview — none of your hardware is touched."*
  - 🚧 Option to run the installer (`dragonfly install`) via UI trigger or CLI.

--- 

### 🛠️ Scenario C: Installation (`dragonfly install`)

#### If Dragonfly is already installed:
✅ Completed, needs unit tests
- Check that all system services are healthy.
- Validate floating IP is reachable and not conflicting.
  - If IP conflict is detected:
    - Offer user a chance to change it.
    - Reassign IP using `kube-vip` without disrupting the system.

#### If Dragonfly is **not** installed:
🚧 In progress, needs unit tests

- Start the installation **immediately** *and* launch the **Install UI**.
- 🚧 Smee is disabled during install to prevent interference.
- Install proceeds with the following steps:

##### 🚀 Installation Phases:

1. **Floating IP assignment**

   - Scans next 20 IPs above host's IP.
   - Claims first available address.

2. **Cluster setup**

   - Deploys `k3s` if not already running.
   - 🚧 Waits for it to pass `livez` / `readyz` checks (or CoreDNS).
   - Grabs kubeconfig.

3. **Chart deployment**

   - Deploys modified Tinkerbell stack (Smee & Hook downloads disabled).
   - NGINX for Tinkerbell points to Dragonfly port 3000.
   - Deploys Dragonfly chart, which gives you a real live Dragonfly install on the floating IP selected earlier.

4. **Visual Feedback**
🚀 Completed!
   - Shows live animated rocket UI with install phases via SSE.
   - Updates status messages for each step ("Installing k3s", "Deploying Tinkerbell", etc).

5. **Transition to main UI**
🚧🚧🚧 Heavy construction 🚧🚧🚧
   - After install completes: 
     - UI fades and rehydrates from the live server.
     - Seamless redirect to live Dragonfly instance. 
     - As this is now a Scenario B system with no selected mode:
       - The Welcome screen is shown.
       - If an Xbox controller is connected:
         - Middle (Flight) card is focused and expands.
         - Pressing A activates Flight mode immediately.

---

### 🚀 Scenario D: Mode Selection (Simple, Flight, Swarm)

When the user selects a mode from the Welcome screen:

#### 🟢 Simple Mode:
- Deploy a lightweight DHCP helper daemon.
- This acts similarly to Smee, but without interfering with existing DHCP servers.
- Uses Dragonfly’s MAC-to-IPXE selector infrastructure.
- Can staple iPXE instructions onto existing DHCP replies.
- Slower but agentless — great for legacy environments.

#### 🟡 Flight Mode:

- Deploy Smee in full automatic mode.
- Bootstrap all required OS templates immediately.
- PXE boots are automatically registered and provisioned.
- Designed for fast, hands-free cluster bring-up.

#### 🔵 Swarm Mode:

- Does everything Flight Mode does.
- Also enables:
  - `k3s` bootstrap token creation
  - Auto-join scripts for new machines
  - UI changes:
    - "Convert to Dragonfly node" button appears on unjoined nodes.
    - Enables node-based clustering and self-healing behavior.

---

### Scenario E: Just Type
🚧 In progress
The "just type" feature allows users to simply click a text field within the UI and type.

- ✅ When the user clicks a text field, it activates the field so the user can immediately start typing.
- ✅ The field will be empty with the previous text shown in half opacity until the user starts typing.
- ✅ If the user types a new value and clicks away, the field will be ready to save upon pressing an Apply button that will replace the Reimage button on the Actions column for the machine.

#### Field Types
| Field Type | Field Editable? | Field Validation | On Change Result |
|------------|-------------|-------------|-------------|
| ✅ MAC Address | ⚠️ Yes (with confirm) | Regex for MAC (XX:XX:XX:XX:XX:XX) | Modal confirmation before saving |
| ✅ Hostname | ✅ Yes | RFC-1123 / DNS-safe | Inline save, triggers Apply button |
| ✅ Friendly Name | ✅ Yes | N/A | Read-only identity fingerprint, can be overridden |
| 🚧 Tags | ✅ Only when displayed | Freeform or enum | Triggers Apply button |
| 🚧 IP Address | ⚠️ Only when configured as a static IP | N/A | Offers to apply the new IP address on the next reimaging run |
| 🚧 Status | ✅ Yes | Basic state transitions are validated | Displays a tooltip with the new status and the impact the change will have, triggers Apply button

#### Example 1:
✅ Completed, needs unit tests
A user opens the machine list and sees a list of machines.

They click the MAC address field of a machine and type a new MAC address, but accidentally enter an invalid MAC format.

The field will show a tooltip with a suggested valid format and refuse to save the new address.

The user then corrects the MAC address and saves the new address.

Since the MAC address is usually not changed unless you're dealing with a VM, we pop up a modal to confirm the change.

"Changing the MAC address for this machine will cause issues if the machine is still using the old one. Definitely ready to change it?" ✅ (Ed.: We don't use this wording yet, maybe we should)

🚧 The user clicks "Yes" and the new address is saved.

#### Example 2

A user opens the machine list and sees a list of machines.

They click the hostname field of a machine and type a new hostname.

The field will accept any DNS-valid hostname. The user changes the hostname from

'jellyfin' to 'jellyfin01' and clicks save.

The "Reimage" button on the right changes to a calm deep blue "Apply" button.

🚧 Pressing "Apply" will change the hostname of that machine to 'jellyfin01' in future workflows and deployments. If the user clicks and holds Apply, a modal will appear asking if they want to apply the change straight away.

#### Example 3
Status: ✅ Completed, needs unit tests
A user opens the machine list and sees a list of machines.

They click the friendly name field of a machine and type a new friendly name.

The field will accept any friendly name. The user changes the friendly name from the BIP39-style

four word name (`CensusAbleQualityParent`) to `topaz-control-master`.

🚧 The friendly name will be saved and displayed in the machine list.

Changing their mind, the user quickly clicks the friendly name field again and clears the text.

🚧The friendly name will be cleared and the original BIP39-style name will be shown again and saved.

### Scenario F: Reimaging machines

#### Example 1:
A user opens the machine list and sees a list of machines.

They select a different OS than the one currently installed on the machine.

The blue Apply button appears on the Actions column for the machine, replacing the Reimage button.

The user clicks Apply and a modal pops up confirming that the user wants to reimage the machine
to apply the new OS, warning at the loss of data.

🚧 The user clicks "Yes" and the machine is reimaged with the new OS.

🚧 If the user is not using an IPMI capable machine (or Proxmox machine) the chosen machine will have a IPXE option added to the bootloader, allowing the machine to be reimaged without the need for IPMI.

🚧 If the user is using an IPMI capable machine, the chosen machine will be rebooted into PXE mode over IPMI, booted into the agent, rebooted into PXE mode again and then booted into HookOS to install the OS.

#### Example 2:
A user opens the machine list and sees a list of machines.

They click the Reimage button for a machine.

A modal pops up confirming that the user wants to reimage the machine
to apply the new OS, warning at the loss of data.

🚧 The user clicks "Yes" and the machine is reimaged with the new OS.

### Scenario G:

🚧🚧🚧 Heavy construction 🚧🚧🚧
A user opens the machine list, and sees no machines.

They see an Add Machine + button.

They see a list of options for machines they want to add, with a friendly UI like the Welcome UI from welcome.html.

They see the following options:
- Name: Physical machine or virtual machine
  slug: machine
  Description: Any normal machine that can network boot (PXE).
- Name: Proxmox host or cluster
  slug: proxmox
  Description: A Proxmox host running zero or more VMs.
- Name: Cloud host
  slug: cloud
  Description: A cloud host, such as one on DigitalOcean.
- Name: Container host
  slug: container
  Description: A container host, such as Docker, Podman, or LXD.

They select Proxmox host option. An interface pops up that allows them to select from autodiscovered Proxmox clusters in their network (a very lightweight scan of port 8006 on the network) with an option to manually enter the address of a Proxmox host or cluster.

They select a Proxmox cluster and see (selected by default) an option to select all VMs in the cluster and display them in the machine list. There are also options to select no VMs by default (but allow VMs to be selected later), select only VMs with specific tags or sets of tags, or prevented from selecting VMs at all.

They select the "Select all VMs" option and the list of VMs is displayed on the Machines page.

They connect an Xbox controller to their computer and in an instant the UI shapeshifts into a gamepad-friendly interface, with cards that adapt to the size of the user's display - even on a 32:9 ultrawide or a 4K TV.

They see a grid, with 60 VMs displayed in a grid, adapted to the size and shape of their display.

They navigate through the machine list, select 6 VMs, and mark them as Debian 12 servers, and then they install Debian.

Once the machines are deployed, they select them, go to options, and select "Assign role -> Proxmox cluster". The options menu will be a GTA V style radial menu, with a list of roles that the user has enabled or added appearing to allow for quick and easy selection.

They confirm their settings and Proxmox is deployed via PyInfra, then the machines are clustered.

Dragonfly then offers to add the newly created cluster to the list of machines in Dragonfly, offering to add the hosts, the VMs, or both.

They select "Add hosts and VMs" and the hosts and VMs are added to the machine list.

They see the new cluster in the machine list, with the hosts and VMs clearly labelled as such.

The grid displays the hosts and VMs in a grid, with the hosts wrapped around the VMs they manage and control. The grid is arranged Tetris style with a sorting algorithm that tries to fill the screen as best as possible.

### Scenario H:
🚧🚧🚧 Heavy construction 🚧🚧🚧
Let's add real deployment tracking to the demo.

The way it will work is you can run a normal deployment of Dragonfly, install a bunch of machines with it, then shut it down and start the server in demo mode. In demo mode, it will replace the normal list of machines with a fake list of machines and not give out any information to allow real clients to avoid booting from it.

However, it will keep an anonymised set of the previous list of deployment timings and use those to generate fake deployment timings for the demo list of machines. Machines will have a 5% chance per minute of being marked as redeployed, and a 5% chance per minute of being marked as failed.

### Scenario I: Monitoring via Uptime Kuma and CheckMK
🚧🚧🚧 Heavy construction 🚧🚧🚧
A user opens the dashboard and sees a list of their services statuses.

They see that one of their services is currently failing. It is missing two nodes from a three node cluster. They look and see that one of the nodes was marked as not starting on boot by accident, and the other was on a virtualisation host with a failed power supply. They replace the power supply, boot the host, start the other machine and the service comes back online.

🚧 (As a bonus, in "YOLO" mode a local Ollama machine will be in charge of doing an allowlisted list of actions to fix problems as they come up, such as rebooting a machine that failed to boot by accident) 🚧

### Summary

Dragonfly’s onboarding is structured to:

- Be beautiful and expressive from first contact
- Stay safe and read-only in Demo Mode
- Empower users with role selection
- Guide them gently but confidently into full automation

Whether they're starting from nothing or joining an existing stack —
Dragonfly meets them exactly where they are, and takes them further.

