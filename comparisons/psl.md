# Comparison for psl

| **Indicator** | **Rust Implementation** | **Python/Cython** | **Functional Parity** | **Test Coverage Parity** | **Notes** |
|---------------|-------------------------|-------------------|-----------------------|--------------------------|-----------|
| Indicator | Rust Implementation | Python/Cython Implementation | Functional Parity | Test Coverage Parity | Notes |
|-----------|----------------------|-------------------------------|--------------------|-----------------------|-------|
| PSL       | `psl.rs`             | `psl.pxd` (Cython)            | 🟢                 | 🟢                    | Both implementations use identical logic for Parity Symmetric Line calculation, validated against reference data. Tests cover initialization, updates, and edge cases equivalently. |
