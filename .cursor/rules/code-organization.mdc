---
description: Rust code structure and organization standards
globs: ["**/*.rs"]
---

# Code Organization

## Constants

- Place constants below imports with SCREAMING_SNAKE_CASE naming
- Group constants by purpose:
  - Configuration constants
  - Business logic constants
  - Error constants
  - Default values
- Document constants with description, units, and rationale
- Use typed constants where appropriate

## Magic Numbers

- Replace all magic numbers with named constants
- Use PURPOSE_UNIT naming template (e.g., TIMEOUT_SECONDS)
- Relocate all constants to a dedicated constants section

## Structure

- Organize code in logical sections: imports, constants, types, functions
- Keep related functionality together
- Follow rustfmt conventions for consistent layout
- Avoid deeply nested code structures
- Limit function and module size for maintainability