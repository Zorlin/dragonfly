---
description: Rust error handling patterns and strategies
globs: ["**/*.rs"]
---

# Error Handling Patterns

- Use Result<T, E> instead of panicking for recoverable errors
- Create custom error types per module that implement std::error::Error
- Use the ? operator for error propagation with added context
- Handle all error cases explicitly and document failure scenarios

## Common Error Patterns

- @missing_items: Import missing traits or implement required methods
- @type_mismatches: Apply appropriate type conversions
- @unresolved_imports: Check module paths and Cargo.toml dependencies
- @try_operator_errors: Only use ? with Result/Option types
- @trait_bound_failures: Implement or derive required traits
- @ambiguous_items: Use fully qualified syntax to resolve conflicts

## Error Type Implementation

- Custom error types should implement std::error::Error
- Include conversion From implementations for common error types
- Provide context in error messages for better debugging
- Document error types and their specific meanings 