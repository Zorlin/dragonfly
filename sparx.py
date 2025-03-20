import os
import sys
import subprocess
import re
from pathlib import Path
import socket
from textual.app import App, ComposeResult
from textual.widgets import Welcome, Header
from textual import events

WELCOME_MD = """
# Sparx

Sparx is awesome!

> Hello world!
"""

class SparxApp(App):

    def compose(self) -> ComposeResult:
        yield Header()
        yield Container(Static(Markdown(WELCOME_MD), id="text"), id="md")
        yield Button("Continue", id="continue")

    def on_mount(self) -> None:
        self.title = "Sparx"
        self.sub_title = "systems management software"
        self.screen.styles.background = "darkblue"

    def on_button_pressed(self) -> None:
        self.exit()

    COLORS = [
        "white",
        "maroon",
        "red",
        "purple",
        "fuchsia",
        "olive",
        "yellow",
        "navy",
        "teal",
        "aqua",
    ]

    def on_key(self, event: events.Key) -> None:
        if event.key.isdecimal():
            self.screen.styles.background = self.COLORS[int(event.key)]
    pass

def is_darwin():
    return sys.platform == 'darwin'

def run_cmd(cmd, mode="normal"):
    # If silent is True, collect command output but only print it if we hit an error.
    # If forceSilent is True, we don't print anything even in the event of an error.
    if mode == "silent":
        output = subprocess.run(cmd, shell=True, check=True, capture_output=True, text=True)
        if output.returncode != 0:
            print(output.stdout)
    elif mode == "forceSilent":
        subprocess.run(cmd, shell=True, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    else:
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

def is_k0sctl_installed():
    try:

        run_cmd('k0sctl version', mode="forceSilent")
        return True
    except subprocess.CalledProcessError:
        return False
    
def install_k0sctl():
    # Install k0sctl
    # If we're on Linux or WSL, install from the latest GitHub release
    # If we're on macOS, install from Homebrew
    if is_darwin():
        run_cmd('brew install k0sproject/tap/k0sctl')
    else:
        # Parse the latest release from GitHub
        import requests
        import json

        # Get the latest release information
        response = requests.get('https://api.github.com/repos/k0sproject/k0sctl/releases/latest')
        
        if response.status_code == 200:
            data = json.loads(response.text)
            latest_release = data['tag_name']
        else:
            print(f"Failed to fetch latest release of k0sctl: {response.status_code}")
            sys.exit(1)

        # Download the latest release
        run_cmd(f'sudo curl -sSfL https://github.com/k0sproject/k0sctl/releases/download/{latest_release}/k0sctl-linux-amd64 -o /usr/local/bin/k0sctl')
        run_cmd('sudo chmod +x /usr/local/bin/k0sctl')

def build_main_interface():
    app = SparxApp()
    app.run()

def main():
    # Check if k0sctl is installed
    if not is_k0sctl_installed():
        print("Installing k0sctl...")
        install_k0sctl()

    # Show the main interface
    build_main_interface()

if __name__ == "__main__":
    main()