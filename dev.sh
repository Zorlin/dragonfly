#!/bin/bash

# If redeploy is given as an argument
# run k3s-uninstall, then regardless of outcome, run cleanip, then run cargo run -- install

if [ "$1" == "redeploy" ]; then
    k3s-uninstall.sh
    cleanip
    cargo run -- install
elif [ "$1" == "server" ]; then
    # Spin down Dragonfly's statefulset so we can replace it temporarily
    KUBECONFIG=k3s.yaml kubectl scale statefulset dragonfly --replicas=0 -n tink

    # Spin up the web ui
    KUBECONFIG=k3s.yaml cargo run -- server
elif [ "$1" == "cleanserver" ]; then
    # Spin down Dragonfly's statefulset so we can replace it temporarily
    KUBECONFIG=k3s.yaml kubectl scale statefulset dragonfly --replicas=0 -n tink

    # Remove the existing server data
    sudo chown -R $(whoami):$(whoami) /var/lib/dragonfly
    rm -rf /var/lib/dragonfly/*

    # Spin up the web ui
    KUBECONFIG=k3s.yaml cargo run -- server
fi