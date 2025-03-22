import os
import sys
import subprocess
import re
from pathlib import Path
import socket
from typing import List, Optional
from dataclasses import dataclass
from textual.app import App, ComposeResult
from textual.containers import Horizontal, Vertical, Container
from textual.widgets import (
    Header, Footer, Button, Input, Static, DataTable,
    Switch, Label, LoadingIndicator, Welcome, Markdown
)
from textual.binding import Binding
from textual.reactive import reactive
from textual import events
from textual.message import Message
from textual.validation import Length, Number, Function
from textual.keys import Keys
import threading
import time
import json

WELCOME_MD = """
# Sparx

Sparx is awesome!

> Hello world!
"""

@dataclass
class Host:
    """Host data class"""
    name: str
    enabled: bool = True
    connection_status: bool = None  # None means unknown, True means up, False means down
    role: str = "both"
    ip_address: str = ""  # Add the missing ip_address attribute
    
    def __str__(self) -> str:
        return self.name
    
    @property
    def display_name(self) -> str:
        return f"{'✓' if self.enabled else '✗'} {self.name}"
        
    @property
    def role_emoji(self) -> str:
        """Return emoji for role"""
        if self.role == "worker":
            return "💪"  # Worker role
        elif self.role == "controller":
            return "🧠"  # Controller role
        elif self.role == "both":
            return "🤹"  # Both roles
        else:
            return "❓"  # Unknown role

class Colors:
    ORANGE = '\033[0;33m'
    CYAN = '\033[0;36m'
    GREEN = '\033[0;32m'
    GRAY = '\033[0;90m'
    RED = '\033[0;31m'
    NC = '\033[0m'  # No Color

class HostTable(DataTable):
    """A table showing hosts with their status"""
    
    def __init__(self) -> None:
        super().__init__()
        self.cursor_type = "row"
        self.zebra_stripes = True
        self.add_column("✅", width=8)  # Checkbox column
        self.add_column("🌐", width=8)  # Connection status column
        self.add_column("Role", width=8)  # Role column
        self.add_column("Hostname", width=76)  # Adjusted width for new column
    
    def compose(self) -> ComposeResult:
        return []
    
    def update_hosts(self, hosts: List[Host]) -> None:
        """Update the table with host information"""
        self.clear()
        
        # First ensure table has all required columns
        if len(self.columns) < 4:  # Now we need 4 columns
            # Clear any existing columns
            self.columns.clear()
            # Add required columns
            self.add_column("✅", width=8)
            self.add_column("🌐", width=8)
            self.add_column("Role", width=8)
            self.add_column("Hostname", width=76)
        
        # Then add each host row
        for host in hosts:
            # Use checkbox symbols for enabled status
            enabled_status = "☑️" if host.enabled else "☐"
            
            # Add row with all four columns
            self.add_row(
                enabled_status,
                host.connection_status,
                host.role_emoji,
                host.name,
            )
    
    def on_key(self, event: events.Key) -> None:
        """Handle table-specific keyboard navigation"""
        if event.key == "up" or event.key == "down":
            # Don't call super().on_key since it doesn't exist
            # Just update the manager's selected_index based on cursor position
            manager = self.app.query_one(HostManager)
            # Update manager's selection index based on cursor position
            if hasattr(self, "cursor_coordinate") and self.cursor_coordinate is not None:
                row, _ = self.cursor_coordinate
                if 0 <= row < len(manager.hosts):
                    manager.selected_index = row

    def on_mount(self) -> None:
        """When table is mounted, connect to cursor changes"""
        self.watch(self, "cursor_coordinate", self._on_cursor_changed)
    
    def _on_cursor_changed(self, cursor) -> None:
        """React when the cursor position changes in the table"""
        if cursor is not None:
            row, _ = cursor
            # Get the host manager and update its selected index
            try:
                manager = self.app.query_one(HostManager)
                if 0 <= row < len(manager.hosts):
                    manager.selected_index = row
            except Exception:
                pass

class HostInput(Input):
    """An input field for hostnames with validation"""
    
    def __init__(self) -> None:
        super().__init__(
            placeholder="Enter hostname (e.g. server.example.com or server[01-10].example.com)"
        )
    
    def on_input_changed(self) -> None:
        """Handle input validation when the input changes"""
        if not self.value:
            return
            
        if not self.validate_hostname(self.value):
            self.add_class("error")
        else:
            self.remove_class("error")
    
    @staticmethod
    def validate_hostname(value: str) -> bool:
        """Validate hostname format"""
        if not value or not isinstance(value, str):
            return False
        
        # Check if it's a pattern
        if '[' in value and ']' in value:
            # Format: hostname[1-10].domain.com
            pattern1 = r'^[a-zA-Z0-9.-]*\[(\d+)-(\d+)\][a-zA-Z0-9.-]*$'
            # Format: hostname[01-10].domain.com (zero-padded)
            pattern2 = r'^[a-zA-Z0-9.-]*\[(\d+):(\d+)\][a-zA-Z0-9.-]*$'
            
            if re.match(pattern1, value) or re.match(pattern2, value):
                return True
            
            # Additional check for the specific pattern format
            if re.match(r'^.*\[\d+-\d+\].*$', value):
                parts = re.split(r'[\[\]]', value)
                # At least 3 parts: before bracket, inside bracket, after bracket
                if len(parts) >= 3:
                    # Check the range inside brackets
                    range_parts = parts[1].split('-')
                    if len(range_parts) == 2:
                        try:
                            start, end = int(range_parts[0]), int(range_parts[1])
                            # Valid range
                            if start <= end:
                                return True
                        except ValueError:
                            pass
        
        # Check if it's an IP address
        ip_pattern = r'^(\d{1,3}\.){3}\d{1,3}$'
        if re.match(ip_pattern, value):
            try:
                parts = value.split('.')
                return all(0 <= int(part) <= 255 for part in parts)
            except ValueError:
                return False
        
        # Check if it's a valid hostname
        hostname_pattern = r'^[a-zA-Z0-9]([a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?(\.[a-zA-Z0-9]([a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?)*$'
        return bool(re.match(hostname_pattern, value))
    
    def on_key(self, event: events.Key) -> None:
        """Handle keyboard input"""
        if event.key == "enter":
            # If Enter is pressed, find the Add button and trigger it
            add_btn = self.app.query_one("#add-btn")
            add_btn.press()
            # Then refocus on self
            self.focus()
        elif event.key == "right":
            # If right arrow is pressed and cursor is at the end of the input
            if self.cursor_position >= len(self.value):
                # Move to Add button
                self.app.query_one("#add-btn").focus()
                event.prevent_default()
                event.stop()

class HostManager(Static):
    """The main host management interface"""
    
    hosts: reactive[List[Host]] = reactive(list)
    selected_index: reactive[Optional[int]] = reactive(None)
    username: str = ""
    virtual_ip: str = "192.168.122.200/24"  # Default virtual IP
    
    def __init__(self, username: str = "") -> None:
        super().__init__()
        self.username = username
    
    def compose(self) -> ComposeResult:
        with Vertical():
            with Horizontal(id="username-row"):
                yield Label("Username:", id="username-label")
                yield Input(value=self.username, id="username-input", placeholder="Enter username (default: your login)")
            yield HostTable()
            with Horizontal(id="host-input-row"):
                yield HostInput()
                yield Button("Add", id="add-btn", variant="primary")
            with Horizontal(id="vip-row"):
                yield Label("Virtual IP:", id="vip-label")
                yield Input(value=self.virtual_ip, id="vip-input", placeholder="192.168.122.200/24")
            yield Button("Continue", id="continue", variant="primary")
        
        # Load hosts when the component is created
        self.load_hosts()
        
        return [
            # Return the child components
        ]
    
    def on_mount(self) -> None:
        """Called when the widget is mounted"""
        # Load values from existing k0sctl.yaml
        k0sctl_values = self.load_k0sctl_config()
        
        # Get the virtual IP - prioritize existing file over app config
        if 'virtual_ip' in k0sctl_values:
            self.virtual_ip = k0sctl_values['virtual_ip']
            if hasattr(self.app, 'virtual_ip'):
                self.app.virtual_ip = self.virtual_ip
            print(f"Loaded virtual IP {self.virtual_ip} from existing k0sctl.yaml")
        elif hasattr(self.app, 'virtual_ip'):
            self.virtual_ip = self.app.virtual_ip
        
        # Set the virtual IP input field
        try:
            vip_input = self.query_one("#vip-input")
            vip_input.value = self.virtual_ip
        except Exception as e:
            print(f"Error setting virtual IP field: {e}")
        
        # Store auth_pass if found
        if 'auth_pass' in k0sctl_values:
            self.auth_pass = k0sctl_values['auth_pass']
            print(f"Loaded auth_pass from existing k0sctl.yaml")
        
        # Load hosts first
        self.load_hosts()
        
        # Then update the table
        table = self.query_one(HostTable)
        table.update_hosts(self.hosts)
        
        # Schedule connectivity check after hosts are loaded and table is updated
        self.app.call_later(self.check_host_connectivity)
    
    def load_hosts(self) -> None:
        try:
            with open('inventories/remote.py', 'r') as f:
                content = f.read()
                # Parse hosts from Python list syntax
                matches = re.findall(r"'([^']+)'", content)
                self.hosts = []
                for match in matches:
                    if '@' in match:
                        # If we already have a username in the file, preserve it
                        username, hostname = match.split('@', 1)
                        if not self.username:
                            self.username = username
                        self.hosts.append(Host(name=hostname, ip_address=hostname))
                    else:
                        self.hosts.append(Host(name=match, ip_address=match))
            
            # Schedule a DNS resolution for all hosts
            threading.Thread(target=self._resolve_all_hosts, daemon=True).start()
        except FileNotFoundError:
            self.hosts = []
    
    def save_hosts(self) -> None:
        """Save hosts to the inventory file"""
        os.makedirs('inventories', exist_ok=True)
        with open('inventories/remote.py', 'w') as f:
            f.write("hosts = [\n")
            for host in self.hosts:
                if host.enabled:
                    f.write(f"    '{host.name}',  # Role: {host.role}\n")
            f.write("]\n")
        
        # Also save a k0sctl.yaml file for the actual deployment
        self.generate_k0sctl_config()
    
    def detect_ssh_key_path(self):
        """Detect available SSH keys and return the best one based on priority."""
        # Define key types in order of preference
        key_preferences = [
            "id_ed25519",  # Best - modern and secure
            "id_ecdsa",    # Good - still secure but not as modern
            "id_rsa",      # Acceptable if 4096+ bits
            "identity"     # Generic key
        ]
        
        # Check for keys in ~/.ssh/
        ssh_dir = Path.home() / ".ssh"
        if not ssh_dir.exists():
            print(f"SSH directory not found at {ssh_dir}")
            return "~/.ssh/id_rsa"  # Default fallback
        
        # Look for keys in order of preference
        for key_name in key_preferences:
            key_path = ssh_dir / key_name
            if key_path.exists():
                return f"~/.ssh/{key_name}"
        
        # If no well-known keys found, look for any private key files (without .pub extension)
        for file in ssh_dir.glob("*"):
            if file.is_file() and not file.name.endswith(".pub") and not file.name == "known_hosts" and not file.name == "config":
                return f"~/.ssh/{file.name}"
        
        # Default fallback
        return "~/.ssh/id_rsa"

    def generate_k0sctl_config(self) -> None:
        """Generate k0sctl configuration file"""
        if not self.hosts:
            return
        
        # Create k0sctl config directory if it doesn't exist
        os.makedirs('k0sctl', exist_ok=True)
        
        # Load auth_pass and other values from existing config if available
        k0sctl_values = self.load_k0sctl_config()
        
        # Use existing auth_pass or generate a new one
        if 'auth_pass' in k0sctl_values:
            self.auth_pass = k0sctl_values['auth_pass']
        elif not hasattr(self, 'auth_pass') or not self.auth_pass:
            try:
                import secrets
                self.auth_pass = secrets.token_hex(4)
            except ImportError:
                import random
                import string
                random.seed(time.time())
                self.auth_pass = ''.join(random.choices(string.ascii_letters + string.digits, k=8))
        
        # Detect the best SSH key to use
        ssh_key_path = self.detect_ssh_key_path()
        print(f"Using SSH key: {ssh_key_path}")
        
        with open('k0sctl/k0sctl.yaml', 'w') as f:
            f.write("apiVersion: k0sctl.k0sproject.io/v1beta1\n")
            f.write("kind: Cluster\n")
            f.write("metadata:\n")
            f.write("  name: k0s-cluster\n")
            f.write("  user: admin\n")
            f.write("spec:\n")
            f.write("  hosts:\n")
            
            for host in self.hosts:
                if host.enabled:
                    # First determine the role
                    if host.role == "worker":
                        f.write(f"  - role: worker\n")
                    elif host.role == "controller":
                        f.write(f"  - role: controller\n")
                    elif host.role == "both":
                        f.write(f"  - role: controller+worker\n")

                    # Add SSH configuration with IP address
                    f.write(f"    ssh:\n")
                    f.write(f"      address: {host.ip_address}\n")
                    f.write(f"      user: {self.username}\n")
                    f.write(f"      port: 22\n")
                    f.write(f"      keyPath: {ssh_key_path}\n")
            
            self.k0s_version = "v1.31.6+k0s.0"

            # Add k0s configuration
            f.write("  k0s:\n")
            f.write("    version: " + self.k0s_version + "\n")
            f.write("    config:\n")
            f.write("      apiVersion: k0s.k0sproject.io/v1beta1\n")
            f.write("      kind: ClusterConfig\n")
            f.write("      metadata:\n")
            f.write("        name: k0s-cluster\n")
            f.write("      spec:\n")
            f.write("        telemetry:\n")
            f.write("          enabled: false\n")
            f.write("        network:\n")
            f.write("          controlPlaneLoadBalancing:\n")
            f.write("            enabled: true\n")
            f.write("            type: Keepalived\n")
            f.write("            keepalived:\n")
            f.write("              vrrpInstances:\n")
            f.write(f"                - virtualIPs: [\"{self.virtual_ip}\"]\n")
            f.write(f"                  authPass: {self.auth_pass}\n")
            f.write(f"                  virtualRouterID: 78\n")
            f.write("          nodeLocalLoadBalancing:\n")
            f.write("            enabled: true\n")
            f.write("            type: EnvoyProxy\n")
            
            print(f"K0sctl configuration saved to k0sctl/k0sctl.yaml")
    
    def update_table(self, preserve_selection=None) -> None:
        """Update the host table and preserve selection if specified"""
        table = self.query_one(HostTable)
        table.update_hosts(self.hosts)
        
        # Restore selection if needed
        if preserve_selection is not None and 0 <= preserve_selection < len(self.hosts):
            self.selected_index = preserve_selection
            table.cursor_coordinate = (preserve_selection, 0)
        # Otherwise, only update if needed
        elif self.selected_index is not None and self.selected_index >= len(self.hosts):
            # Selection was out of bounds, fix it
            self.selected_index = len(self.hosts) - 1 if self.hosts else None
            if self.selected_index is not None:
                table.cursor_coordinate = (self.selected_index, 0)
        
        # Don't automatically start connectivity checks to avoid circular reference
        # We'll explicitly call it instead
    
    def update_buttons(self) -> None:
        # No visible buttons to update, but we keep the method for future use
        pass
    
    def on_button_pressed(self, event: Button.Pressed) -> None:
        try:
            # Check for the button ID first
            button_id = event.button.id if hasattr(event, 'button') and hasattr(event.button, 'id') else None
            
            if button_id == "continue":
                # Set flag on the app instance to confirm user wants to continue
                self.app.user_confirmed_continue = True
                self.app.exit()
                return

            if button_id == "add-btn":
                input_widget = self.query_one(HostInput)
                input_value = input_widget.value
                
                if not input_value:
                    self.notify("Hostname cannot be empty", severity="error")
                    return
                    
                if not input_widget.validate_hostname(input_value):
                    self.notify("Invalid hostname format", severity="error")
                    return
                    
                # Valid input, expand host patterns
                try:
                    new_hosts = expand_host_pattern(input_value)
                    if not new_hosts:
                        self.notify("No hosts generated from pattern", severity="error")
                        return
                    
                    # Check for duplicates
                    existing_hostnames = [h.name.lower() for h in self.hosts]
                    duplicates = []
                    
                    # Filter out duplicates
                    hosts_to_add = []
                    for host in new_hosts:
                        if host.lower() in existing_hostnames:
                            duplicates.append(host)
                        else:
                            hosts_to_add.append(host)
                    
                    # Notify about duplicates if any were found
                    if duplicates:
                        if len(duplicates) == len(new_hosts):
                            self.notify(f"All hosts already exist: {', '.join(duplicates)}", severity="error")
                            return
                        else:
                            self.notify(f"Skipping duplicate hosts: {', '.join(duplicates)}", severity="warning")
                    
                    # Add the hosts that aren't duplicates
                    for host_name in hosts_to_add:
                        # Try to resolve the hostname to an IP immediately
                        ip_address = host_name  # Default to hostname
                        try:
                            # Check if it's an IP first
                            if re.match(r'^(\d{1,3}\.){3}\d{1,3}$', host_name):
                                ip_address = host_name
                            else:
                                # Try to resolve hostname to IP
                                ip_address = socket.gethostbyname(host_name)
                                print(f"Resolved {host_name} to {ip_address}")
                        except Exception as e:
                            print(f"Could not resolve {host_name}: {e}")
                            # Keep using hostname as IP address if resolution fails
                        
                        self.hosts.append(Host(name=host_name, ip_address=ip_address))

                    input_widget.value = ""
                    self.save_hosts()
                    self.update_table()  # This will also trigger connectivity checks
                except Exception as e:
                    self.notify(f"Error expanding host pattern: {str(e)}", severity="error")
                    
            # When toggling a host's enabled status
            if button_id == "toggle-host":
                if self.selected_index is not None and 0 <= self.selected_index < len(self.hosts):
                    host = self.hosts[self.selected_index]
                    host.enabled = not host.enabled
                    
                    # Update just the enabled cell
                    table = self.query_one(HostTable)
                    enabled_status = "☑️" if host.enabled else "☐"
                    table.update_cell(self.selected_index, 0, enabled_status)
                    
                    self.save_hosts()

        except Exception as e:
            print(f"Error in button press handler: {e}")
            self.notify(f"Error processing button: {str(e)}", severity="error")
    
    def on_data_table_row_selected(self, event: DataTable.RowSelected) -> None:
        try:
            # Validate that row_key exists and is not None
            if event.row_key is None:
                self.notify("Invalid row selection", severity="error")
                self.selected_index = None
                return
                
            # Process the row key
            if hasattr(event.row_key, "value"):
                if event.row_key.value is not None:
                    self.selected_index = int(event.row_key.value)
                else:
                    self.selected_index = None
                    return
            else:
                try:
                    self.selected_index = int(event.row_key)
                except (TypeError, ValueError):
                    # If we can't convert to int, just use the index directly
                    # but don't try to validate it with numeric operations
                    self.selected_index = event.row_key
                    self.update_buttons()
                    return
            
            # Only validate if we have a numeric index
            if isinstance(self.selected_index, (int, float)) and not (0 <= self.selected_index < len(self.hosts)):
                self.notify("Invalid row selection", severity="error")
                self.selected_index = None
                
            self.update_buttons()
        except Exception as e:
            # Log the error but don't crash
            print(f"Error handling row selection: {e}")
            self.notify(f"Error selecting row: {str(e)}", severity="error")
            self.selected_index = None

    def on_input_changed(self, event: Input.Changed) -> None:
        """Handle input changes"""
        if event.input.id == "username-input":
            self.username = event.value
            self.app.host_username = event.value
            
            # Save the updated username to config immediately
            try:
                config = load_config()
                config["username"] = event.value
                save_config(config)
            except Exception as e:
                print(f"Error saving config: {e}")
                # Don't notify the user as this is non-critical
        elif event.input.id == "vip-input":
            self.virtual_ip = event.value
            self.app.virtual_ip = event.value
            
            # Save the virtual IP to config
            try:
                config = load_config()
                config["virtual_ip"] = event.value
                save_config(config)
            except Exception as e:
                print(f"Error saving config: {e}")
    
    def on_key(self, event: events.Key) -> None:
        """Handle key events"""
        key = event.key
        
        # If we're in the host table
        if self.query_one(HostTable).has_focus:
            table = self.query_one(HostTable)
            
            # If up key is pressed and we're at the top of the list
            if key == "up" and (self.selected_index is None or self.selected_index == 0):
                # Move focus to the username field
                self.query_one("#username-input").focus()
                event.stop()
                return
                
            # If down key is pressed and we're at the bottom of the list
            if key == "down" and self.selected_index is not None and self.selected_index >= len(self.hosts) - 1:
                # Move focus to the hostname input field
                self.query_one(HostInput).focus()
                event.stop()
                return
        
        # If we're in the hostname input and up is pressed
        if self.query_one(HostInput).has_focus and key == "up":
            # Move focus to the bottom of the host table
            table = self.query_one(HostTable)
            if len(self.hosts) > 0:
                self.selected_index = len(self.hosts) - 1
                table.focus()
                event.stop()
                return
        
        # If we're in the username input and down is pressed
        if self.query_one("#username-input").has_focus and key == "down":
            # Move focus to the host table
            table = self.query_one(HostTable)
            if len(self.hosts) > 0:
                self.selected_index = 0
                table.focus()
                event.stop()
                return
                
        # If we're in the Add button and down is pressed
        if self.query_one("#add-btn").has_focus and key == "down":
            # Move focus to the virtual IP field
            self.query_one("#vip-input").focus()
            event.stop()
            return
            
        # If we're in the Virtual IP field and up is pressed
        if self.query_one("#vip-input").has_focus and key == "up":
            # Move focus to the Add button
            self.query_one("#add-btn").focus()
            event.stop()
            return
            
        # If we're in the Virtual IP field and down is pressed
        if self.query_one("#vip-input").has_focus and key == "down":
            # Move focus to the Continue button
            self.query_one("#continue").focus()
            event.stop()
            return
            
        # If we're in the Continue button and up is pressed
        if self.query_one("#continue").has_focus and key == "up":
            # Move focus to the Virtual IP field
            self.query_one("#vip-input").focus()
            event.stop()
            return

    def _set_table_cursor_to_first(self):
        """Helper to set the table cursor to the first row after focus change"""
        table = self.query_one(HostTable)
        if len(self.hosts) > 0:
            table.cursor_coordinate = (0, 0)
            self.selected_index = 0

    def update_selection(self):
        """Ensure the table cursor position matches selected_index"""
        table = self.query_one(HostTable)
        if self.selected_index is not None and 0 <= self.selected_index < len(self.hosts):
            table.cursor_coordinate = (self.selected_index, 0)

    def action_toggle_host(self) -> None:
        """Enable/disable the selected host"""
        if self.selected_index is not None and 0 <= self.selected_index < len(self.hosts):
            # Store the current selection before making changes
            current_selection = self.selected_index
            
            # Toggle the host
            host = self.hosts[self.selected_index]
            host.enabled = not host.enabled
            
            # Save and update with preserved selection
            self.save_hosts()
            self.update_table(preserve_selection=current_selection)

    def check_host_connectivity(self) -> None:
        """Check connectivity for all hosts"""
        # Make sure the hosts list is not empty
        if not self.hosts:
            return
        
        print("Starting connectivity checks for", len(self.hosts), "hosts")    
        # Mark all hosts as being checked
        for host in self.hosts:
            host.connection_status = "⏳"  # Start with hourglass
        
        # Update table to show hourglasses
        self.update_table(preserve_selection=self.selected_index)
        
        # Run checks in background thread
        thread = threading.Thread(target=self._check_connectivity, daemon=True)
        thread.start()
    
    def _check_connectivity(self) -> None:
        """Background thread to check connectivity for all hosts"""
        print("Connectivity check thread started")
        for idx, host in enumerate(self.hosts):
            print(f"Checking host {idx+1}/{len(self.hosts)}: {host.name}")
            # Try DNS lookup first
            try:
                ip = socket.gethostbyname(host.name)
                print(f"DNS lookup for {host.name} succeeded: {ip}")
                
                # Store the resolved IP address in the host
                host.ip_address = ip
                
                # Try connecting to SSH port (22)
                try:
                    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
                    s.settimeout(1.0)  # 1 second timeout
                    result = s.connect_ex((ip, 22))
                    s.close()
                    
                    # Update the status based on connection result
                    if result == 0:
                        print(f"SSH connection to {host.name} succeeded")
                        host.connection_status = "✅"  # Connected successfully
                    else:
                        print(f"SSH connection to {host.name} failed with code {result}")
                        host.connection_status = "❌"  # Failed to connect
                except Exception as e:
                    print(f"Exception during SSH connection to {host.name}: {str(e)}")
                    host.connection_status = "❌"  # Exception during connection
            except Exception as e:
                # DNS lookup failed
                print(f"DNS lookup failed for {host.name}: {str(e)}")
                host.connection_status = "⚠️"  # Warning symbol for DNS failure
                # Keep the hostname as IP if resolution fails
                host.ip_address = host.name
            
            # Update the UI with the new status
            try:
                self.app.call_from_thread(self._update_host_status, idx, host)
                print(f"Updated status for {host.name} to {host.connection_status}")
            except Exception as e:
                print(f"Error updating UI for {host.name}: {str(e)}")
    
    def _update_host_status(self, idx, host):
        """Update a single host's status in the table (called from main thread)"""
        try:
            # First update our data model
            if idx < len(self.hosts):
                self.hosts[idx].connection_status = host.connection_status
            
            # Then update the UI
            table = self.query_one(HostTable)
            # Verify both row and column exist before updating
            if idx < table.row_count:
                try:
                    # Try with explicit coordinates first
                    table.update_cell_at((idx, 1), host.connection_status)
                except Exception as e:
                    print(f"Couldn't update at coordinates, trying with get_cell: {e}")
                    try:
                        # Alternative approach
                        row = table.get_row_at(idx)
                        # Update the connection status column (column index 1)
                        table.update_cell(row.key, 1, host.connection_status)
                    except Exception as e2:
                        print(f"Alternative approach failed too: {e2}")
                print(f"UI cell updated for {host.name}: {host.connection_status}")
            else:
                print(f"Row {idx} doesn't exist in table with {table.row_count} rows")
        except Exception as e:
            print(f"Error in _update_host_status: {str(e)}")

    def action_toggle_role(self) -> None:
        """Toggle the role of the selected host"""
        if self.selected_index is not None and 0 <= self.selected_index < len(self.hosts):
            # Store the current selection before making changes
            current_selection = self.selected_index
            
            # Cycle through roles: worker -> controller -> both -> worker
            host = self.hosts[self.selected_index]
            if host.role == "worker":
                host.role = "controller"
            elif host.role == "controller":
                host.role = "both"
            else:
                host.role = "worker"
            
            # Save and update with preserved selection
            self.save_hosts()
            self.update_table(preserve_selection=current_selection)
            
            # Try to update just the role cell using a more robust approach
            try:
                table = self.query_one(HostTable)
                try:
                    # Try with explicit coordinates first
                    table.update_cell_at((self.selected_index, 2), host.role_emoji)
                except Exception as e:
                    print(f"Couldn't update role cell at coordinates: {e}")
                    try:
                        # Alternative approach
                        row = table.get_row_at(self.selected_index)
                        # Find the role column index by checking column labels
                        role_column = None
                        for idx, column in enumerate(table.columns):
                            if column.label.plain == "Role" or column.label.plain == "👤":
                                role_column = idx
                                break
                        
                        if role_column is not None:
                            table.update_cell(row.key, role_column, host.role_emoji)
                        else:
                            print("Role column not found, table update will use default refresh")
                    except Exception as e2:
                        print(f"Alternative approach for role update failed: {e2}")
            except Exception as e:
                print(f"Error updating role cell: {e}")
                # Full table update already happened above, so we'll still see the change

    def load_k0sctl_config(self):
        """Load values from existing k0sctl.yaml if it exists"""
        try:
            if os.path.exists('k0sctl/k0sctl.yaml'):
                with open('k0sctl/k0sctl.yaml', 'r') as f:
                    yaml_content = f.read()
                    # Dictionary to store found values
                    found_values = {}
                    
                    # Look for the virtualIPs line
                    for line in yaml_content.split('\n'):
                        line = line.strip()
                        if 'virtualIPs:' in line:
                            # Extract the IP address from the line
                            # Format should be like: - virtualIPs: ["192.168.122.200/24"]
                            start = line.find('"')
                            if start != -1:
                                end = line.find('"', start + 1)
                                if end != -1:
                                    found_values['virtual_ip'] = line[start + 1:end]
                    
                        if 'authPass:' in line:
                            # Extract the auth password
                            # Format should be like: authPass: SomePassword
                            parts = line.split('authPass:')
                            if len(parts) > 1:
                                found_values['auth_pass'] = parts[1].strip()
                    
                    return found_values
        except Exception as e:
            print(f"Error loading values from k0sctl.yaml: {e}")
        
        return {}

    def _resolve_all_hosts(self):
        """Background thread to resolve all hostnames to IPs"""
        for host in self.hosts:
            if host.ip_address == host.name:  # Only resolve if not already an IP
                try:
                    # Check if it's an IP already
                    if re.match(r'^(\d{1,3}\.){3}\d{1,3}$', host.name):
                        continue  # Already an IP
                    
                    # Try to resolve hostname to IP
                    ip = socket.gethostbyname(host.name)
                    print(f"Resolved {host.name} to {ip}")
                    host.ip_address = ip
                except Exception as e:
                    print(f"Could not resolve {host.name}: {e}")
                    # Keep using hostname as fallback

class SparxApp(App):
    host_username: str = ""
    install_type: str = "remote"  # Keep this for backward compatibility
    # Flag to track if user clicked Continue
    user_confirmed_continue: bool = False
    virtual_ip: str = "192.168.122.200/24"  # Default virtual IP
    
    CSS = """
    Screen {
        background: $background;
    }
    
    Header {
        height: 1;
        background: $boost;
        color: $text;
    }
    
    HostManager {
        height: 100%;
        margin: 0;
        padding: 0;
    }
    
    DataTable {
        height: 70%;  /* Reduced to make room for VIP row */
        border: none;
        margin: 0;
        padding: 0;
    }
    
    Horizontal {
        height: auto;
        margin: 0;
        align: center middle;
    }
    
    #username-row {
        dock: top;
        height: 3;
        padding: 0 1;
        background: $background;
        border: none;
        align-horizontal: left;
    }
    
    #vip-row {
        height: 1;
        padding: 0 0;
        background: $background;
        border: none;
        align-horizontal: left;
        margin-bottom: 0;
    }
    
    #vip-label {
        width: 14;
        height: 1;
        padding: 0 0 0 2;
        content-align: left middle;
        margin-right: 0;
    }
    
    #vip-input {
        width: 20;
        height: 1;
        border: none;
        padding: 0 0;
        background: $surface;
        margin-left: 0;
    }
    
    #username-label {
        width: 10;
        padding: 1 0;
        content-align: right middle;
        margin-right: 1;
    }
    
    #username-input {
        width: 100%;
        border: none;
        padding: 1 0;
        background: $background;
        margin-left: 0;
    }
    
    #host-input-row {
        height: 3;
        align: left middle;
        padding: 0 1;
        margin-top: 1;
        margin-bottom: 1;
    }
    
    HostInput {
        width: 85%;
        margin-right: 1;
    }
    
    Input.error {
        border: solid red;
    }
    
    Button {
        margin: 0 1;
    }

    Button#continue {
        dock: bottom;
        width: 100%;
        margin: 0;
        height: 3;
    }
    
    Footer {
        background: $boost;
        color: $text;
        dock: bottom;
        height: 2;
        border-top: none;
        padding: 0;
    }
    
    #footer-roles {
        background: $boost;
        color: $text;
        dock: bottom;
        height: 1;
        width: 100%;
        content-align: center middle;
    }
    """
    
    BINDINGS = [
        Binding("q", "quit", "Quit"),
        Binding("a", "add_host", "Add Host"),
        Binding("r", "remove_host", "Remove Host"),
        Binding("e", "toggle_host", "Enable/Disable Host"),
        Binding("t", "toggle_role", "Toggle Role"),
        Binding("tab", "focus_next", "Next Field"),
        Binding("shift+tab", "focus_previous", "Previous Field"),
        Binding("c", "press_continue", "Continue"),
        Binding(Keys.Up, "move_up", "Arrow keys to navigate"),
        Binding(Keys.Down, "move_down", ""),
        Binding(Keys.Right, "move_right", ""),
        Binding(Keys.Left, "move_left", "")
    ]

    def compose(self) -> ComposeResult:
        yield Header()
        yield HostManager(username=self.host_username)
        with Container(id="footer-roles"):
            yield Label("Role Key - Worker 💪 - Control Plane 🧠 - Both 🤹")
        yield Footer()

    def on_mount(self) -> None:
        self.title = "Sparx"
        self.sub_title = "systems management software"
        # Set initial focus to the username field
        self.set_focus(self.query_one("#username-input"))
        
        # Start connectivity checks after a short delay
        def check_connectivity(_timer=None):
            manager = self.query_one(HostManager)
            manager.check_host_connectivity()
        
        # Schedule the check after UI is fully loaded
        self.call_later(check_connectivity, 0.5)
    
    def action_add_host(self) -> None:
        self.query_one(HostInput).focus()
    
    def action_remove_host(self) -> None:
        manager = self.query_one(HostManager)
        if manager.selected_index is not None:
            # Directly remove the host
            if 0 <= manager.selected_index < len(manager.hosts):
                del manager.hosts[manager.selected_index]
                manager.selected_index = None
                manager.save_hosts()
                manager.update_table()
    
    def action_toggle_host(self) -> None:
        """Enable/disable the selected host"""
        # Get the host manager
        manager = self.query_one(HostManager)
        if manager.selected_index is not None and 0 <= manager.selected_index < len(manager.hosts):
            # Toggle the host
            host = manager.hosts[manager.selected_index]
            host.enabled = not host.enabled
            
            # Save and update with preserved selection
            manager.save_hosts()
            manager.update_table(preserve_selection=manager.selected_index)

    def action_press_continue(self) -> None:
        """Simulate pressing the continue button"""
        self.user_confirmed_continue = True
        self.exit()

    def action_move_up(self) -> None:
        """Move selection up in the host table"""
        # Get the focused element
        focused = self.screen.focused
        
        # If we're already in the table or table is focused, navigate within table
        if isinstance(focused, DataTable) or (hasattr(focused, "parent") and isinstance(focused.parent, DataTable)):
            table = self.query_one(HostTable)
            manager = self.query_one(HostManager)
            
            if len(manager.hosts) == 0:
                # No hosts, move to username field
                self.query_one("#username-input").focus()
                return
                
            # If at the top of the list, move to username field
            if manager.selected_index == 0 or manager.selected_index is None:
                self.query_one("#username-input").focus()
                return
            
            # Otherwise move up one in the table
            if isinstance(manager.selected_index, int):
                new_index = (manager.selected_index - 1) % len(manager.hosts)
                # Update selection
                table.cursor_coordinate = (new_index, 0)
                manager.selected_index = new_index
                manager.update_buttons()
                return
        # Otherwise use normal field navigation
        elif isinstance(focused, HostInput):
            # Move from hostname input to host table
            manager = self.query_one(HostManager)
            table = self.query_one(HostTable)
            table.focus()
            if len(manager.hosts) > 0:
                idx = len(manager.hosts) - 1
                table.cursor_coordinate = (idx, 0)
                manager.selected_index = idx
            return
        elif focused and focused.id == "add-btn":
            # From add button to host input
            self.query_one(HostInput).focus()
            return
        elif focused and focused.id == "continue":
            # From continue button to add button
            self.query_one("#add-btn").focus()
            return
        # Let default navigation handle it if no specific case was matched
        self.screen.focus_previous()
    
    def action_move_down(self) -> None:
        """Move selection down in the host table"""
        # Get the focused element
        focused = self.screen.focused
        
        # If username is focused, move to table
        if focused and focused.id == "username-input":
            manager = self.query_one(HostManager)
            table = self.query_one(HostTable)
            table.focus()
            if len(manager.hosts) > 0:
                table.cursor_coordinate = (0, 0)
                manager.selected_index = 0
            return
        
        # If we're already in the table or table is focused, navigate within table
        if isinstance(focused, DataTable) or (hasattr(focused, "parent") and isinstance(focused.parent, DataTable)):
            table = self.query_one(HostTable)
            manager = self.query_one(HostManager)
            
            if len(manager.hosts) == 0:
                # If no hosts, move to hostname input
                self.query_one(HostInput).focus()
                return
                
            # If at the bottom of the list, move to hostname input
            if manager.selected_index == len(manager.hosts) - 1 or manager.selected_index is None:
                self.query_one(HostInput).focus()
                return
            
            # Otherwise move down one in the table
            if isinstance(manager.selected_index, int):
                new_index = (manager.selected_index + 1) % len(manager.hosts)
                # Update selection
                table.cursor_coordinate = (new_index, 0)
                manager.selected_index = new_index
                manager.update_buttons()
                return
        # From host input to add button
        elif isinstance(focused, HostInput):
            # From host input to add button
            self.query_one("#add-btn").focus()
            return
        elif focused and focused.id == "add-btn":
            # From add button to continue button
            self.query_one("#continue").focus()
            return
        # Otherwise let default navigation handle it if no specific case was matched
        self.screen.focus_next()
        
    def action_move_right(self) -> None:
        """Handle right arrow navigation"""
        focused = self.screen.focused
        
        if isinstance(focused, HostInput) and hasattr(focused, "cursor_position") and hasattr(focused, "value"):
            # If cursor is at the end of input, move to add button
            if focused.cursor_position >= len(focused.value):
                self.query_one("#add-btn").focus()
                return
        elif focused and focused.id == "add-btn":
            # From add button to continue button
            self.query_one("#continue").focus()
            return
        
    def action_move_left(self) -> None:
        """Handle left arrow navigation"""
        focused = self.screen.focused
        
        if focused and focused.id == "add-btn":
            # From add button to host input
            self.query_one(HostInput).focus()
            return
        elif focused and focused.id == "continue":
            # From continue button to add button
            self.query_one("#add-btn").focus()
            return

    def action_toggle_role(self) -> None:
        """Toggle the role of the selected host"""
        manager = self.query_one(HostManager)
        manager.action_toggle_role()

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
    
    # Handle pattern with numeric range like server[1-10].example.com
    match = re.search(r'(.*)\[(\d+)-(\d+)\](.*)', pattern)
    if match:
        prefix, start, end, suffix = match.groups()
        # Check if we need to preserve leading zeros
        if start.startswith('0') and len(start) > 1:
            # Preserve leading zeros
            width = len(start)
            return [f"{prefix}{str(i).zfill(width)}{suffix}" for i in range(int(start), int(end) + 1)]
        else:
            # No leading zeros
            return [f"{prefix}{i}{suffix}" for i in range(int(start), int(end) + 1)]
    
    # Handle pattern with explicit zero-padding format like server[01:10].example.com
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

def get_config_path():
    """Get the path to the config file."""
    config_dir = Path.home() / '.config' / 'sparx'
    config_dir.mkdir(parents=True, exist_ok=True)
    return config_dir / 'sparx.json'

def load_config():
    """Load the config from the file."""
    config_path = get_config_path()
    if config_path.exists():
        try:
            with open(config_path, 'r') as f:
                return json.load(f)
        except json.JSONDecodeError:
            # If the file is corrupted, return default config
            return {"username": os.getlogin()}
    return {"username": os.getlogin()}

def save_config(config):
    """Save the config to the file."""
    config_path = get_config_path()
    with open(config_path, 'w') as f:
        json.dump(config, f)

def main():
    # Check if k0sctl is installed
    if not is_k0sctl_installed():
        print("Installing k0sctl...")
        install_k0sctl()

    # Load config to get the saved username
    config = load_config()
    username = config.get("username", os.getlogin())
    
    # Always use remote deployment as default (user can add localhost if they want local)
    default_install_type = 'remote'
    
    # Show the host management UI
    app = SparxApp()
    app.host_username = username
    app.install_type = default_install_type
    
    try:
        # Run the app
        app.run()
        
        # Save the username after the app exits
        config["username"] = app.host_username
        save_config(config)
        
        # Check if the user explicitly confirmed continuation via Continue button
        if not app.user_confirmed_continue:
            print(f"{Colors.ORANGE}Thanks for using Sparx!{Colors.NC}")
            return
        
    except KeyboardInterrupt:
        # Save the username even on keyboard interrupt
        config["username"] = app.host_username
        save_config(config)
        print(f"{Colors.ORANGE}Thanks for using Sparx!{Colors.NC}")
        return
    except Exception as e:
        print(f"{Colors.RED}Error running the UI: {str(e)}{Colors.NC}")
        print(f"{Colors.RED}Cannot continue with deployment due to UI error.{Colors.NC}")
        return
    
    # We only get here if the user explicitly confirmed by clicking Continue
    
    # Check if the k0sctl config file exists
    if not os.path.exists('k0sctl/k0sctl.yaml'):
        print(f"{Colors.RED}Error: k0sctl configuration file not found.{Colors.NC}")
        print(f"{Colors.RED}Cannot continue with deployment.{Colors.NC}")
        return
        
    # Final confirmation
    print(f"{Colors.GREEN}All prerequisites met. Ready to run deployment.{Colors.NC}")
    confirm = input(f"{Colors.CYAN}Are you ABSOLUTELY sure you want to run the deployment now? (yes/no): {Colors.NC}")
    
    if confirm.lower() != "yes":
        print(f"{Colors.ORANGE}Deployment cancelled by user.{Colors.NC}")
        return
        
    # Run k0sctl
    print(f"{Colors.GREEN}Running k0s deployment with k0sctl...{Colors.NC}")
    try:
        run_cmd("k0sctl apply --config k0sctl/k0sctl.yaml")
        print(f"{Colors.GREEN}Deployment completed successfully!{Colors.NC}")
        
        # Get kubeconfig for the user
        print(f"{Colors.GREEN}Retrieving kubeconfig...{Colors.NC}")
        kubeconfig_dir = os.path.expanduser("~/.kube")
        os.makedirs(kubeconfig_dir, exist_ok=True)
        
        run_cmd("k0sctl kubeconfig --config k0sctl/k0sctl.yaml > ~/.kube/config")
        os.chmod(os.path.expanduser("~/.kube/config"), 0o600)
        
        print(f"{Colors.GREEN}Kubeconfig saved to ~/.kube/config{Colors.NC}")
        print(f"{Colors.GREEN}You can now use kubectl to interact with your cluster!{Colors.NC}")
        
    except subprocess.CalledProcessError as e:
        print(f"{Colors.RED}Error during deployment: {str(e)}{Colors.NC}")
        return

def test_host_validation():
    """Test hostname validation logic"""
    # Valid hostnames
    assert HostInput.validate_hostname("example.com") is True
    assert HostInput.validate_hostname("sub.example.com") is True
    assert HostInput.validate_hostname("192.168.1.1") is True
    assert HostInput.validate_hostname("server[01-10].example.com") is True
    
    # Invalid hostnames
    assert HostInput.validate_hostname("") is False
    assert HostInput.validate_hostname("invalid..com") is False
    assert HostInput.validate_hostname("256.256.256.256") is False
    assert HostInput.validate_hostname("server[1-a].example.com") is False
    
    print("Host validation tests passed!")

def test_host_pattern_expansion():
    """Test host pattern expansion logic"""
    # Test simple hostname
    assert expand_host_pattern("example.com") == ["example.com"]
    
    # Test numeric range
    assert expand_host_pattern("host[1-3].com") == [
        "host1.com",
        "host2.com",
        "host3.com"
    ]
    
    # Test zero-padded range
    assert expand_host_pattern("host[01-03].com") == [
        "host01.com",
        "host02.com",
        "host03.com"
    ]

# Run tests when called directly
if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--test":
        # Run tests
        print("Running tests...")
        test_host_validation()
        test_host_pattern_expansion()
        print("All tests passed!")
        sys.exit(0)
    
    main()
