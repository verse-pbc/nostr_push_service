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

## Unblocking Workflow

When you hit a blocker, follow these steps in order:

1. **Build‑check**
   ```bash
   cargo check
   ```
   Quickly catch compilation errors without producing a binary.

2. **Inspect a dependency's source**
   ```bash
   inspectcrate.sh <crate‑version> <search‑term>
   ```
   To open the first match in your editor:
   ```bash
   CRATE="nostr-sdk-0.40.0"
   QUERY="relay"
   entry=$(inspectcrate.sh $CRATE $QUERY | jq '.[0]')
   file=$(echo $entry | jq -r '.file')
   line=$(echo $entry | jq -r '.lines[0]')
   $EDITOR +$line "$file"
   ```

3. **Run tests**
   ```bash
   cargo test
   ```
   Fail fast after big refactors to catch broken code.

4. **Review local docs**
   ```bash
   tree docs/
   $EDITOR docs/
   ```
   Browse your `docs/` folder for additional context.

5. **Check recent changes**
   ```bash
   git diff
   ```
   Spot unintended regressions or logic changes since your last commit.

For more detailed development information, see [Developer Guide](docs/developer-guide.md).