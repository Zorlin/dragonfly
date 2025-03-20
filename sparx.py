import os
import sys
import subprocess
import re
from pathlib import Path
import socket

# ANSI color codes
class Colors:
    ORANGE = '\033[0;33m'
    CYAN = '\033[0;36m'
    GREEN = '\033[0;32m'
    GRAY = '\033[0;90m'
    RED = '\033[0;31m'
    NC = '\033[0m'  # No Color

# Emoji symbols
class Symbols:
    CHECK = "✅"
    CROSS = "❌"
    EDIT = "✏️ "
    PLUS = "➕"
    LIST = "📋"
    STOP = "🚫"

def is_darwin():
    return sys.platform == 'darwin'

def run_cmd(cmd):
    subprocess.run(cmd, shell=True, check=True)

def check_ssh_keys():
    try:
        result = subprocess.run(['ssh-add', '-l'], capture_output=True, text=True)
        if result.returncode != 0:
            print("No SSH keys found in ssh-agent. Please add your SSH key and rerun this script.")
            print("You can add your key using: ssh-add ~/.ssh/your_key")
            print("If you do not have an SSH key, you can generate one "
                  "by running 'ssh-keygen -t ed25519' "
                  "and entering a reasonable passphrase that you can record in a password manager.")
            sys.exit(1)

        # Check for strong keys
        if not any(key in result.stdout for key in ['ED25519', 'ECDSA', 'RSA']):
            print("No secure SSH keys found in ssh-agent. Please add a secure key (ED25519, ECDSA, or RSA 4096+) and rerun this script.")
            sys.exit(1)
    except FileNotFoundError:
        print("ssh-add command not found. Please ensure SSH is installed.")
        sys.exit(1)

def configure_ssh():
    ssh_config = Path.home() / '.ssh' / 'config'
    
    # Check if config exists and search for any valid variant of the setting
    if ssh_config.exists():
        content = ssh_config.read_text()
        # Match any combination of spaces/tabs, optional =, and spaces/tabs around accept-new
        pattern = r'StrictHostKeyChecking[\s=]+accept-new'
        has_setting = bool(re.search(pattern, content, re.IGNORECASE))
    else:
        has_setting = False

    if not has_setting:
        print(f"\n{Colors.CYAN}Your SSH configuration does not have StrictHostKeyChecking set to accept-new.")
        print(f"This setting allows automatic acceptance of new host keys while still protecting against key changes.{Colors.NC}")
        add_config = input("Would you like to add this setting to your SSH config? (y/n): ")
        
        if add_config.lower() == 'y':
            ssh_config.parent.mkdir(exist_ok=True)
            with ssh_config.open('a') as f:
                f.write("\n# Added by bootstrap script - automatically accept new host keys\n")
                f.write("Host *\n")
                f.write("    StrictHostKeyChecking accept-new\n")
            ssh_config.chmod(0o600)
            print(f"{Colors.GREEN}SSH configuration updated successfully{Colors.NC}")
        else:
            print(f"{Colors.CYAN}Please be aware you may need to manually confirm host keys during installation{Colors.NC}")

def check_host_connectivity(hostname):
    try:
        socket.gethostbyname(hostname)
        return True
    except socket.error:
        return False

def expand_host_pattern(pattern):
    if '[' not in pattern or ']' not in pattern:
        return [pattern]
    
    match = re.search(r'(.*)\[(\d+)-(\d+)\](.*)', pattern)
    if match:
        prefix, start, end, suffix = match.groups()
        return [f"{prefix}{i}{suffix}" for i in range(int(start), int(end) + 1)]
    
    match = re.search(r'(.*)\[(\d+):(\d+)\](.*)', pattern)
    if match:
        prefix, start, end, suffix = match.groups()
        width = len(start)
        return [f"{prefix}{str(i).zfill(width)}{suffix}" for i in range(int(start), int(end) + 1)]
    
    return [pattern]

def manage_hosts():
    hosts = []
    while True:
        os.system('clear')
        print(f"\n{Colors.CYAN}=== Host Management {Symbols.LIST} ==={Colors.NC}")
        print("\nCurrent hosts:")
        if not hosts:
            print(f"{Colors.CYAN}  No hosts added yet{Colors.NC}")
        else:
            for i, host in enumerate(hosts, 1):
                print(f"{Colors.GREEN}  {i}. {host}{Colors.NC}")
        
        print("\nOptions:")
        print(f"{Symbols.PLUS} [a] add host    {Symbols.EDIT} [e] edit host")
        print(f"{Symbols.CROSS} [r] remove host {Symbols.CHECK} [d] finish editing")
        print(f"{Symbols.STOP} [q] quit")
        print(f"{Colors.CYAN}Choose an option:{Colors.NC}")
        
        option = input().lower()
        
        if option == 'q':
            print(f"\n{Colors.CYAN}Thanks for using Sparx. Quitting...{Colors.NC}")
            sys.exit(0)
        
        elif option == 'a':
            print(f"\n{Colors.CYAN}Enter host (supports patterns like chaos[1-9] or chaos[01:10].riff.cc):{Colors.NC}")
            while True:
                new_host = input()
                print(f"\n{Colors.CYAN}Checking host connectivity...{Colors.NC}")
                if new_host == 'cancel':
                    break
                if check_host_connectivity(new_host.split('[')[0]):
                    hosts.extend(expand_host_pattern(new_host))
                    break
                print(f"{Colors.RED}Host is not contactable.{Colors.NC}")
                print(f"{Colors.ORANGE}Would you like to add it anyways? (y/n){Colors.NC}")
                add_host = input().lower()
                if add_host == 'y':
                    hosts.extend(expand_host_pattern(new_host))
                    break
                print(f"{Colors.CYAN}Enter host again or enter 'cancel' to cancel.{Colors.NC}")
        
        elif option == 'r':
            if not hosts:
                print(f"\n{Colors.CYAN}No hosts to remove{Colors.NC}")
                input()
                continue
            print(f"\n{Colors.CYAN}Enter number to remove:{Colors.NC}")
            try:
                num = int(input())
                if 1 <= num <= len(hosts):
                    hosts.pop(num - 1)
            except ValueError:
                pass
        
        elif option == 'e':
            if not hosts:
                print(f"\n{Colors.CYAN}No hosts to edit{Colors.NC}")
                input()
                continue
            print(f"\n{Colors.CYAN}Enter number to edit:{Colors.NC}")
            try:
                num = int(input())
                if 1 <= num <= len(hosts):
                    print(f"{Colors.CYAN}Enter new value:{Colors.NC}")
                    new_value = input()
                    hosts[num - 1] = new_value
            except ValueError:
                pass
        
        elif option == 'd':
            if not hosts:
                print(f"\n{Colors.CYAN}Please add at least one host{Colors.NC}")
                input()
                continue
            return hosts

def main():
    if is_darwin():
        print(f"\n{Colors.ORANGE}Friendly notice: running locally is not supported on macOS as it lacks k0s support{Colors.NC}")
        print("Will assume you want to install remotely.")
        install_type = 'remote'
    else:
        print("\nChoose installation type:")
        print(f"{Colors.CYAN}[l] local{Colors.NC}")
        print(f"{Colors.CYAN}[r] remote{Colors.NC}")
        
        while True:
            choice = input("Enter your choice: ").lower()
            if choice in ['l', 'local', 'r', 'remote']:
                install_type = 'local' if choice in ['l', 'local'] else 'remote'
                break
            print("Invalid choice. Please enter 'l' or 'r', or 'local' or 'remote'.")

    if install_type == 'local':
        if is_darwin():
            print(f"{Colors.RED}Error: Local installation is not supported on macOS.{Colors.NC}")
            print("Please choose remote installation instead.")
            return
        run_cmd('pyinfra inventories/local.py bootstrap/k0s.py')
    else:
        username = input("Enter the username for the machine (default: your username): ") or os.getlogin()
        check_ssh_keys()
        configure_ssh()
        hosts = manage_hosts()

        # Create inventory file for pyinfra
        with open('inventories/remote.py', 'w') as f:
            f.write("hosts = [\n")
            for host in hosts:
                f.write(f"    '{username}@{host}',\n")
            f.write("]\n")

        os.environ['SHOW_INSTALL'] = 'true'
        run_cmd(f"pyinfra --ssh-user {username} inventories/remote.py bootstrap/k0s.py")
        os.environ['SHOW_INSTALL'] = 'false'

if __name__ == "__main__":
    main()