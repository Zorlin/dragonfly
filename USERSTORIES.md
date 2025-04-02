# User stories

## Dragonfly Server Deployment

### Overview

This document outlines the core user flows and behaviors for Dragonfly across various scenarios. It defines how Dragonfly behaves when executed from the CLI or server context, what installation looks like, and how the onboarding experience adapts based on state. It also describes the different operating modes available to the user post-install.

---

### 🧪 Scenario A: CLI Invocation (e.g., `dragonfly`)

#### Default behavior:

When a user runs `dragonfly` from the command line:

#### If Dragonfly is already installed:

- Ensure all Dragonfly services are running.
- Print the local address of the WebUI (e.g., `http://localhost:3000`).
- Print the full CLI help text.

#### If Dragonfly is not installed:

- Print the CLI help text.
- Do **not** attempt to inspect any server state.
- Emphasize that `dragonfly install` is the next step.

---

### 🖥️ Scenario B: Server Mode (`DRAGONFLY_SERVER_MODE=true`)

#### If Dragonfly is already installed:

##### If a mode has been selected previously:

- Boot straight into the active mode (Simple, Flight, or Swarm).
- If login enforcement is enabled, present the login screen.

#### If no mode has been selected yet:

- Display the `welcome.html` onboarding screen.

#### If Dragonfly is **not** installed:

- Boot into a full **Demo Experience**:
  - Fake data, in-memory only.
  - UI fully interactive.
  - Banner: *"Demo Mode: Dragonfly is not installed. This is a preview — none of your hardware is touched."*
  - Option to run the installer (`dragonfly install`) via UI trigger or CLI.

---

### 🛠️ Scenario C: Installation (`dragonfly install`)

#### If Dragonfly is already installed:

- Check that all system services are healthy.
- Validate floating IP is reachable and not conflicting.
  - If IP conflict is detected:
    - Offer user a chance to change it.
    - Reassign IP using `kube-vip` without disrupting the system.

#### If Dragonfly is **not** installed:

- Start the installation **immediately** *and* launch the **Install UI**.
- Smee is disabled during install to prevent interference.
- Install proceeds with the following steps:

##### 🚀 Installation Phases:

1. **Floating IP assignment**

   - Scans next 20 IPs above host's IP.
   - Claims first available address.

2. **Cluster setup**

   - Deploys `k3s` if not already running.
   - Waits for it to pass `livez` / `readyz` checks (or CoreDNS).
   - Grabs kubeconfig.

3. **Chart deployment**

   - Deploys modified Tinkerbell stack (Smee & Hook downloads disabled).
   - NGINX for Tinkerbell points to Dragonfly port 3000.
   - Deploys Dragonfly chart, which gives you a real live Dragonfly install on the floating IP selected earlier.

4. **Visual Feedback**

   - Shows live animated rocket UI with install phases via SSE.
   - Updates status messages for each step ("Installing k3s", "Deploying Tinkerbell", etc).

5. **Transition to main UI**

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

### Summary

Dragonfly’s onboarding is structured to:

- Be beautiful and expressive from first contact
- Stay safe and read-only in Demo Mode
- Empower users with role selection
- Guide them gently but confidently into full automation

Whether they're starting from nothing or joining an existing stack —
Dragonfly meets them exactly where they are, and takes them further.

