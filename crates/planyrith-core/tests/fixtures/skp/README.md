# Native golden for SKP interchange

`cube.plyr` is a 1 m cube generated entirely by Planyrith's core commands,
under the project's MIT/Apache-2.0 license. Reproduce it with
`cargo run -p planyrith-core --example generate-skp-fixture`.

The optional converter's tests use it as input for both SKP writers and check
the returned native geometry, scale and topology. Original externally authored
SKP fixtures and their GPL license remain in the separate `planyrith-skp`
component; the core and application binaries do not embed those files.
