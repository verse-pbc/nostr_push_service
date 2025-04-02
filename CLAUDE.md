# Plur Push Service Development Guide

## Build & Test Commands
```bash
# Build the project
cargo build

# Run the application
cargo run

# Run tests
cargo test

# Run a specific test
cargo test test_name

# Run tests with logs visible
RUST_LOG=debug cargo test

# Check code without building
cargo check
```

## Code Style Guidelines
- **Imports**: Group by category (std > external > internal), alphabetize within groups
- **Error Handling**: Use the `ServiceError` enum with `thiserror` for typed errors
- **Types**: Use strong typing with descriptive names; leverage `Option<T>` and `Result<T, E>`
- **Naming**: Use snake_case for functions/variables, CamelCase for types/traits
- **Modules**: One module per file, organized by functionality (service boundaries)
- **Logging**: Use `tracing` macros with appropriate log levels
- **Async**: Use `tokio` for async runtime, properly handle task spawning and cancellation
- **Configuration**: Use environment variables for secrets, settings.toml for defaults
- **Documentation**: Document public functions and modules with /// commented