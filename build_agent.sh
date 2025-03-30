#!/bin/bash
set -e # Exit immediately if a command exits with a non-zero status.

SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )
WORKSPACE_ROOT="$SCRIPT_DIR"
AGENT_CRATE_DIR="$WORKSPACE_ROOT/crates/dragonfly-agent"
AGENT_DOCKERFILE="$AGENT_CRATE_DIR/Dockerfile"
IMAGE_NAME="dragonfly-agent:latest"
OUTPUT_BINARY_NAME="dragonfly-agent"
OUTPUT_PATH="$WORKSPACE_ROOT/$OUTPUT_BINARY_NAME"

echo "Building Docker image '$IMAGE_NAME' from '$AGENT_DOCKERFILE'..."

# Build the Docker image
docker build --platform linux/amd64 -f "$AGENT_DOCKERFILE" -t "$IMAGE_NAME" "$WORKSPACE_ROOT"

if [ $? -ne 0 ]; then
    echo "Error: Docker build failed."
    exit 1
fi

echo "Docker image built successfully."
echo "Copying '$OUTPUT_BINARY_NAME' binary from image..."

# Create a temporary container to copy the file
CONTAINER_ID=$(docker create "$IMAGE_NAME")

if [ -z "$CONTAINER_ID" ]; then
    echo "Error: Failed to create temporary container."
    exit 1
fi

# Copy the binary from the container to the workspace root
docker cp "$CONTAINER_ID:/usr/local/bin/$OUTPUT_BINARY_NAME" "$OUTPUT_PATH"

if [ $? -ne 0 ]; then
    echo "Error: Failed to copy binary from container."
    docker rm -f "$CONTAINER_ID" > /dev/null
    exit 1
fi

# Clean up the temporary container
docker rm -f "$CONTAINER_ID" > /dev/null

echo "Successfully copied '$OUTPUT_BINARY_NAME' to '$OUTPUT_PATH'"

# Optional: Make the copied binary executable
chmod +x "$OUTPUT_PATH"
echo "Made '$OUTPUT_PATH' executable."

echo "Build process complete."
exit 0 