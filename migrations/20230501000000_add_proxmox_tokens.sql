-- Add columns for storing different Proxmox API tokens
ALTER TABLE proxmox_settings ADD COLUMN vm_create_token TEXT DEFAULT NULL;
ALTER TABLE proxmox_settings ADD COLUMN vm_power_token TEXT DEFAULT NULL;
ALTER TABLE proxmox_settings ADD COLUMN vm_config_token TEXT DEFAULT NULL;
ALTER TABLE proxmox_settings ADD COLUMN vm_sync_token TEXT DEFAULT NULL; -- Token for synchronization operations 