#!/usr/bin/env python3

from proxmoxer import ProxmoxAPI
import argparse
import sys
import time

# --- Configuration ---
DEFAULT_NODE = "leeroy"          # Change to your Proxmox node name
DEFAULT_STORAGE = "local-lvm"     # Change to your storage backend
BRIDGE = "vmbr0"                  # Your network bridge name
IMAGE_SIZE_GB = 32                # Size of the VM disk in GB
MEMORY_MB = 4096                  # RAM in MB
VCPUS = 4                         # Number of vCPUs
BOOT_ORDER = "order=scsi0;net0"              # PXE boot first
MACHINE_TYPE = "q35"
BIOS_TYPE = "ovmf"
CPU_TYPE = "host"

# --- Argument Parsing ---
parser = argparse.ArgumentParser(description="Spawn N Proxmox test VMs via API")
parser.add_argument("-H", "--host", required=True, help="Proxmox host/IP")
parser.add_argument("-u", "--user", required=True, help="Proxmox username (e.g. root@pam)")
parser.add_argument("-p", "--password", required=True, help="Proxmox password or API token secret")
parser.add_argument("-n", "--nodes", type=int, required=True, help="Number of test nodes to create")
parser.add_argument("--verify-ssl", action="store_true", help="Verify SSL cert")
args = parser.parse_args()

# --- Connect to Proxmox ---
try:
    proxmox = ProxmoxAPI(
        args.host,
        user=args.user,
        password=args.password,
        verify_ssl=args.verify_ssl
    )
except Exception as e:
    print(f"Error connecting to Proxmox API: {e}")
    sys.exit(1)

# --- Get next available VMID ---
def get_next_vmid():
    return proxmox.cluster.nextid.get()

# --- Create a VM ---
def create_vm(vmid, name):
    print(f"Creating VM {name} (VMID {vmid})...")

    try:
        proxmox.nodes(DEFAULT_NODE).qemu.create(
            vmid=vmid,
            name=name,
            memory=MEMORY_MB,
            cores=VCPUS,
            sockets=1,
            cpu=CPU_TYPE,
            net0=f"virtio,bridge={BRIDGE}",
            boot="order=net0",  # FIXED: valid boot string
            scsihw="virtio-scsi-pci",
            scsi0=f"{DEFAULT_STORAGE}:32",  # FIXED: valid format
            ostype="l26",
            agent=1,
            efidisk0=f"{DEFAULT_STORAGE}:0,efitype=4m,format=raw",
            bios=BIOS_TYPE,
            machine=MACHINE_TYPE
        )
        print(f"✓ VM {name} created successfully.")
    except Exception as e:
        print(f"✗ Failed to create VM {name}: {e}")
        sys.exit(1)

# --- Main logic ---
for i in range(1, args.nodes + 1):
    name = f"testnode{i:02d}"
    vmid = get_next_vmid()
    create_vm(vmid, name)
    time.sleep(0.5)  # Slight delay between VM creations
