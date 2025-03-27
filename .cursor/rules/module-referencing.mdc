---
description: Rust module referencing standards and patterns
globs: ["**/*.rs"]
---

# Module Referencing Standards

## Module References

- Use @module.rs format to reference other modules
- Examples:
  - @module.rs
  - @current_module.rs
  - @other_module.rs
  - @lib.rs
  - @mod.rs

## Import Syntax

Use proper import syntax:
- `use crate::path` for absolute paths within the crate
- `use super::path` for parent module paths
- `use self::path` for current module paths
- `pub use` for re-exporting

## Module Structure

Define modules using consistent patterns:
- `mod name;` for external modules
- `pub mod name;` for public external modules
- `mod name { ... }` for inline modules 