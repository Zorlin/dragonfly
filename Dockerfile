# Use the official Rust image.
FROM rust:slim-bookworm AS builder

# Set the working directory in the container.
WORKDIR /app

# Install dependencies.
RUN apt-get update && apt-get install -y \
    build-essential \
    libssl-dev \
    pkg-config \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Install Node.JS 23.x via NodeSource
RUN bash -c 'curl -fsSL https://deb.nodesource.com/setup_23.x | bash -' && \
    apt-get install -y nodejs

# Copy the current directory contents into the container.
COPY . .

RUN npm install

# Build the application.
RUN cargo build --release

# New container
FROM rust:slim-bookworm AS runner

# Copy the binary from the builder container.
COPY --from=builder /app/target/release/dragonfly /usr/local/bin/dragonfly
# Copy static assets to /opt/dragonfly/
COPY --from=builder /app/crates/dragonfly-server/static /opt/dragonfly/static
# Copy templates to /opt/dragonfly/
COPY --from=builder /app/crates/dragonfly-server/templates /opt/dragonfly/templates

# Expose the port that the application will run on.
EXPOSE 3000

# Set the entrypoint for the container.
ENTRYPOINT ["dragonfly"]
