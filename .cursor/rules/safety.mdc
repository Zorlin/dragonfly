---
description: Rust safety guidelines and best practices
globs: ["**/*.rs"]
---

# Safety Guidelines

## Unsafe Code

- Justify any unsafe code with explicit safety guarantees
- Document invariants and risks for unsafe blocks
- Prefer safe alternatives whenever possible
- Include review requirements for unsafe code

## Safe Practices

- Avoid unwrap() and expect() in production code
- Use checked operations instead of operations that can panic
- Properly handle integer overflow
- Validate input data
- Follow ownership and borrowing rules
- Test edge cases thoroughly

## Error Handling Safety

- Prefer Result over panic for error handling
- Document all possible error cases
- Use ? operator for clean error propagation
- Create custom error types for domain-specific errors 