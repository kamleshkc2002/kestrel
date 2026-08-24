use kestrel_core::{CapabilityStatus, FeatureSpec};

fn main() {
    let feature = FeatureSpec::new(
        "core.capabilities",
        "Capability status",
        CapabilityStatus::Supported,
    );

    println!(
        "Kestrel architecture scaffold: {} ({})",
        feature.label, feature.id
    );
}
