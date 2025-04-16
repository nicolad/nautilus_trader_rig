# Comparison for bollinger_bands

| **Indicator** | **Rust Implementation** | **Python/Cython** | **Functional Parity** | **Test Coverage Parity** | **Notes** |
|---------------|-------------------------|-------------------|-----------------------|--------------------------|-----------|
| Indicator          | Rust Implementation | Python/Cython Implementation | Functional Parity | Test Coverage Parity | Notes                                                                 |
|--------------------|---------------------|------------------------------|-------------------|-----------------------|-----------------------------------------------------------------------|
| Bollinger Bands    | `bollinger_bands.rs`| `bollinger_bands.pyx`        | 🟢                | 🟢                    | Same core logic (SMA + StdDev), matching parameters. Cython optimized with zero-overflow checks. |
