#!/bin/sh
set -e

TINK_SERVER_URL="http://10.7.1.30:42113"  # your Tinkerbell server (adjust as needed)
WORKER_ID="sparx-$(hostname)"
DEVICE=$(ip route | awk '/default/ {print $5}' | head -n1)
MAC=$(cat /sys/class/net/${DEVICE}/address)
IP=$(ip -4 -o addr show dev ${DEVICE} | awk '{print $4}' | cut -d/ -f1)

# Optional: generate UUID
HW_ID=$(uuidgen)

# Optional: name based on MAC
NAME="sparx-$(echo $MAC | tr -d ':' | cut -c1-6)"

echo "Registering ${NAME} with MAC ${MAC} and IP ${IP}"

# --- Create hardware JSON ---
cat > /tmp/hardware.json <<EOF
{
  "id": "${HW_ID}",
  "metadata": {
    "facility": {
      "facility_code": "onprem"
    }
  },
  "network": {
    "interfaces": [
      {
        "dhcp": {
          "mac": "${MAC}",
          "hostname": "${NAME}",
          "ip": {
            "address": "${IP}",
            "netmask": "255.255.255.0",
            "gateway": "10.7.1.1"
          }
        },
        "netboot": {
          "allow_pxe": true,
          "allow_workflow": true
        }
      }
    ]
  }
}
EOF

# --- Submit to Tinkerbell ---
curl -X POST "$TINK_SERVER_URL/hardware" \
     -H "Content-Type: application/json" \
     --data-binary @/tmp/hardware.json

echo "✅ Registered with Tinkerbell."
