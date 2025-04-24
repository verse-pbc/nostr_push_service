# Unblocking Workflow

When you hit a blocker, follow these steps in order:

1. **Build‑check**
    ```bash
    cargo check
    ```
    Quickly catch compilation errors without producing a binary.

2. **Inspect a dependency’s source**
    ```bash
    inspectcrate.sh <crate‑version> <search‑term>
    ```
    This outputs JSON like:
    ```json
    [
      {
        "file": "/Users/daniel/.cargo/registry/src/.../src/client/builder.rs",
        "lines": [12,13]
      },
      {
        "file": "/Users/daniel/.cargo/registry/src/.../src/client/error.rs",
        "lines": [10,11,17,...]
      },
      ...
    ]
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
    Browse your `docs/` folder (design notes, ADRs, spec fragments) for additional context.

5. **Check recent changes**
    ```bash
    git diff
    ```
    Spot unintended regressions or logic changes since your last commit.