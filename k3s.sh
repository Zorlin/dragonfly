#!/usr/bin/env bash
set -euo pipefail

# --- Ask for desired public IP ---
read -rp "🌐 Enter the bootstrap IP for this node (e.g. 10.7.1.30): " BOOTSTRAP_IP

# --- Install k3s ---
if ! command -v k3s &>/dev/null; then
  echo "📦 Installing k3s (single-node)..."
  curl -sfL https://get.k3s.io | INSTALL_K3S_EXEC="--disable traefik" sh -
  echo "✅ k3s installed."
else
  echo "⏩ k3s already installed. Skipping."
fi

# Copy the kubeconfig to the current directory
sudo cp /etc/rancher/k3s/k3s.yaml .
sudo chown $(whoami) k3s.yaml

export KUBECONFIG=$(pwd)/k3s.yaml

# --- Wait for node to be ready ---
echo "⏳ Waiting for Kubernetes node to become ready..."
until kubectl get nodes 2>/dev/null | grep -q ' Ready'; do
  echo -n "."
  sleep 2
done
echo -e "\n✅ Kubernetes is ready."

# --- Install Helm ---
if ! command -v helm &>/dev/null; then
  echo "📦 Installing Helm..."
  curl -sSL https://raw.githubusercontent.com/helm/helm/main/scripts/get-helm-3 | bash
  echo "✅ Helm installed."
else
  echo "⏩ Helm already installed. Skipping."
fi

# --- Install Tinkerbell stack ---
echo "🚀 Installing Tinkerbell stack via Helm..."

# Get pod CIDRs for trusted proxies
trusted_proxies=$(kubectl get nodes -o jsonpath='{.items[*].spec.podCIDR}' | tr ' ' ',')
# Set version and IP variables
STACK_CHART_VERSION=0.5.0

echo "📋 Using trusted proxies: ${trusted_proxies}"

# Create Helm values.yaml file
cat <<EOF > values.yaml
global:
  trustedProxies: 
    - ${trusted_proxies}
  publicIP: ${BOOTSTRAP_IP}
smee:
  # http:
  #   osieUrl:
  #     scheme: "http"
  #     host: "boot.netboot.xyz"
  #     port: 443
  #     path: ""
  dhcp:
    allowUnknownHosts: true
    mode: auto-proxy
    httpIPXE:
      scriptUrl:
        scheme: "http"
        host: "10.7.1.200"
        port: 3000
        path: ""
  additionalArgs:
    - "--dhcp-http-ipxe-script-prepend-mac=true"
stack:
  hook:
    enabled: true
    persistence:
      localPersistentVolume:
        path: /opt/tinkerbell/hook
EOF

# Install Tinkerbell stack with trusted proxies configuration
helm upgrade --install tink-stack oci://ghcr.io/tinkerbell/charts/stack \
  --version "$STACK_CHART_VERSION" \
  --create-namespace \
  --namespace tink \
  --wait \
  -f values.yaml

echo "✅ Tinkerbell stack installed successfully in namespace 'tink'"

echo "📡 PXE services should now be available from: http://$BOOTSTRAP_IP:8080"
