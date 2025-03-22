#!/usr/bin/env bash
set -euo pipefail

# --- CONFIG ---
CLUSTER_NAME="sparx"
export KUBECONFIG="$(pwd)/kubeconfig"

# --- Install yq if missing ---
if ! command -v yq &>/dev/null; then
  echo "📦 yq not found, installing the latest version from GitHub..."
  YQ_VERSION=$(curl -s "https://api.github.com/repos/mikefarah/yq/releases/latest" | grep -Po '"tag_name": "\K.*?(?=")')
  sudo wget "https://github.com/mikefarah/yq/releases/download/${YQ_VERSION}/yq_linux_amd64" -O /usr/local/bin/yq
  sudo chmod +x /usr/local/bin/yq
  echo "✅ yq installed."
fi

# --- Install talosctl if missing ---
if ! command -v talosctl &>/dev/null; then
  echo "📦 talosctl not found, installing via talos.dev..."
  curl -sL https://talos.dev/install | sh
  echo "✅ talosctl installed."
fi

# --- Check cluster health via Kubernetes endpoint ---
CLUSTER_STATUS=$(talosctl cluster show --name "$CLUSTER_NAME" 2>/dev/null || echo "")

if echo "$CLUSTER_STATUS" | grep -q "^KUBERNETES ENDPOINT[[:space:]]*$"; then
  echo "📦 Cluster '$CLUSTER_NAME' not found or uninitialized. Creating cluster..."
  talosctl cluster destroy --name "$CLUSTER_NAME" || true
  talosctl cluster create --name "$CLUSTER_NAME" --skip-kubeconfig --wait
else
  echo "⏩ Talos cluster '$CLUSTER_NAME' is healthy. Skipping create."
fi

# --- Extract API endpoint from Talos config ---
TALOS_CONFIG_PATH="$HOME/.talos/config"
API_ENDPOINT=$(yq e ".contexts.$CLUSTER_NAME.endpoints[0]" "$TALOS_CONFIG_PATH")

# --- Create kubeconfig file ---
talosctl kubeconfig --cluster sparx --talosconfig ~/.talos/config --nodes 127.0.0.1

# --- Set KUBECONFIG environment variable ---
export KUBECONFIG="$(pwd)/kubeconfig"

# --- Verify cluster access ---
echo "🔍 Verifying Kubernetes cluster access..."
if kubectl get nodes -o wide; then
  echo "✅ Successfully connected to Talos Kubernetes cluster '$CLUSTER_NAME'"
  echo "🚀 Your Kubernetes cluster is ready to use!"
  NODE_COUNT=$(kubectl get nodes --no-headers | wc -l)
  echo "   📊 Cluster info: $NODE_COUNT node(s) provisioned"
  
  # Optional: Display more cluster info
  echo "   💻 Control plane endpoint: $API_ENDPOINT"
  echo "   📋 Kubeconfig location: $KUBECONFIG"
else
  echo "❌ Failed to connect to Kubernetes cluster. Please check your configuration."
  exit 1
fi
