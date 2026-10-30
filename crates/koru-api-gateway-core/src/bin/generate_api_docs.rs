// SPDX-License-Identifier: MPL-2.0

//! Regenerate the checked-in public and internal API reference artifacts in
//! `reference/api/` (`cargo run -p koru-api-gateway-core --bin generate_api_docs`).

fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let api = root.join("reference/api");
    std::fs::create_dir_all(&api).expect("create reference/api");
    write_json(
        &api.join("koru-api-gateway.openapi.json"),
        &koru_api_gateway_core::public_openapi(),
    );
    write_json(
        &api.join("koru-api-gateway.asyncapi.json"),
        &koru_api_gateway_core::public_asyncapi(),
    );
    std::fs::write(
        root.join("reference/api/API_REFERENCE.md"),
        koru_api_gateway_core::markdown_reference(),
    )
    .expect("write public Markdown reference");
    std::fs::write(
        root.join("reference/api/API_REFERENCE_INTERNAL.md"),
        koru_api_gateway_core::internal_markdown_reference(),
    )
    .expect("write internal Markdown reference");
    std::fs::write(
        api.join("koru-api-mobile-ffi.md"),
        koru_api_gateway_core::mobile_ffi_reference(),
    )
    .expect("write mobile FFI reference");
}

fn write_json(path: &std::path::Path, value: &serde_json::Value) {
    // Dependency feature unification can enable serde_json's preserve_order.
    // Keep generated references stable regardless of which packages are built.
    let mut value = value.clone();
    value.sort_all_objects();
    let mut bytes = serde_json::to_vec_pretty(&value).expect("serialize generated API document");
    bytes.push(b'\n');
    std::fs::write(path, bytes).expect("write generated API document");
}
