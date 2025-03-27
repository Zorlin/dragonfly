---
description: Rust implementation approach and validation
globs: ["**/*.rs", "Cargo.toml", "Cargo.lock"]
---

# Implementation Approach

## Implementation Philosophy

- Implement over remove: prefer fixing unused code rather than deleting
- Use small changes where possible
- Focus on error resolution rather than code removal
- Preserve existing functionality while fixing issues
- Maintain code organization and structure

## Validation Requirements

- Validate changes with:
  - cargo check: Ensure code compiles
  - cargo clippy: Check for lints and common issues
  - cargo test: Verify functionality works as expected
  - cargo fmt: Maintain code style
- Ensure no regressions after changes
- Verify no new errors are introduced

## Priority-Based Fixing

Fix issues in this order:
1. Build errors (compilation failures)
2. Safety issues (unsafe code, panics)
3. Test failures (broken functionality)
4. Style issues (formatting, naming) 