use kestrel_core::{CapabilityReport, CapabilityStatus, FeatureSpec};
use kestrel_platform::StaticCapabilityProbe;
use kestrel_services::FeatureRegistry;

fn main() {
    let feature = FeatureSpec::new(
        "core.capabilities",
        "Capability status",
        CapabilityStatus::Supported,
    );
    let probe = StaticCapabilityProbe::new(
        CapabilityReport::new(
            feature.id,
            CapabilityStatus::Supported,
            "The Phase 1 composition scaffold is available.",
        )
        .with_selected_backend("In-memory scaffold"),
    );
    let mut registry = FeatureRegistry::default();
    registry
        .register_probe(feature.clone(), &probe)
        .expect("the composition scaffold must use matching feature IDs");

    println!(
        "Kestrel Phase 1 scaffold: {} feature registered ({})",
        registry.registrations().len(),
        feature.id
    );
}
