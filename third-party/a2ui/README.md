# A2UI schema references

The three JSON schemas in the ignored `v1_0/` directory are fetched unchanged
from [a2ui-project/a2ui](https://github.com/a2ui-project/a2ui), revision
`84474c34f151f19eaebd1f416065b027722adc15`, under Apache-2.0.
`third-party/fetch.manifest` records their exact SHA-256 pins. They are upstream
reference inputs, not a runtime framework, vendored implementation, or promise
of full A2UI conformance.

Koru's first-party bounded parser and model schema are in
`crates/koru-ai-core/src/mini_apps.rs` and `mini_apps/create.schema.json`.
They implement one creation message with five fixed domain components, no
functions/data model/content mirroring, and a 4,096-byte declaration limit.
The catalog names remain a feature-branch proposal pending maintainer approval.
