# Stage 1: Build the application
FROM rust:1.85.1 as builder

# Install build dependencies
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy manifests and lock file
COPY Cargo.toml Cargo.lock ./

# Create dummy src/main.rs to cache dependencies
RUN mkdir src && echo "fn main(){}" > src/main.rs

# Build dependencies (this layer is cached if manifests don't change)
RUN cargo build --release --bin nostr_push_service

# Copy the actual source code
COPY . .

# Build the application binary, leveraging cached dependencies
RUN touch src/main.rs && cargo build --release --bin nostr_push_service

# Stage 2: Create the final lean image
FROM debian:bookworm-slim

# Install runtime dependencies (use libssl3 for bookworm)
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*

# Set working directory
WORKDIR /app

# Copy the compiled binary from the builder stage
COPY --from=builder /app/target/release/nostr_push_service .

# Copy the configuration directory
COPY config ./config

# Expose any ports the application listens on (if applicable, currently none directly)
# EXPOSE 8080

# Set the user (optional, but good practice)
# RUN useradd -ms /bin/bash appuser
# USER appuser

# Define the entrypoint
ENTRYPOINT ["./nostr_push_service"]

# Default command (can be overridden)
CMD []