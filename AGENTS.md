# Ricochet project guidance

- Read `docs/ricochet-design.md` before making architectural or implementation changes.
- Treat the design brief as the current project direction, but distinguish its firm constraints from its exploratory language features and open questions.
- Preserve documented BBC BASIC V/VI source behavior and public SWI contracts where feasible. Do not change an external contract without stating the reason and documenting the compatibility path.
- Keep the Rust runtime a hosted, kernel-like core unless the project explicitly adopts a different scope. Do not introduce bare-metal kernel or device-driver assumptions by default.
- Keep task addresses logical and caller-scoped. Do not expose unchecked host pointers through BASIC or SWI interfaces.
- Prefer BASIC64 for desktop behavior and higher-level OS policy; reserve Rust for runtime mechanisms and foundational services.
- When a decision resolves an open question or changes the architecture, update the design brief and explain the decision in the change.
