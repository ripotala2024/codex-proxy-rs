use codex_proxy_protocol_bridge::{manifest, plugin};

#[test]
fn manifest_and_handler_are_loadable() {
    let manifest = manifest().expect("manifest must parse");
    assert_eq!(manifest.name, "protocol-bridge");
    plugin().expect("plugin handler must match the manifest");
}
