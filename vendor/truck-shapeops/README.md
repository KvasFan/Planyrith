# truck-shapeops 0.4.0

Source: https://crates.io/crates/truck-shapeops/0.4.0
Upstream: https://github.com/ricosjp/truck
License: Apache-2.0, included in LICENSE-APACHE.

Local change (2026-10-28): the `truck-meshalgo` dependency disables default
features and selects analyzers, filters and tessellation explicitly. Its VTK
backend otherwise introduces a C compression library. All Rust algorithms are
unchanged. The unused experimental `alternative` module allows `dead_code`
so warnings-as-errors builds can compile this path dependency. This copy is
excluded from the application workspace checks.
