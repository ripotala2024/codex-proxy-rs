use gateway_plugin_sdk::manifest::Manifest;

pub fn manifest() -> Result<Manifest, gateway_plugin_sdk::manifest::ManifestError> {
    Manifest::from_author_slice(include_bytes!("../../plugin.json"))
}
