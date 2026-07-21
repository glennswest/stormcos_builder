//! The reconciler — issue-driven, level-triggered image builds.
//!
//! stormcos_builder does not watch commits. It watches its OWN issue queue for
//! "a component build succeeded" signals filed by component-builder, harvests
//! the resulting releases, and builds a new stormcos image only when the
//! harvested set actually changed.
//!
//! Level-triggered, not edge-triggered: every cycle it SCANS its open
//! `build-ok` issues rather than relying on catching each as it is filed — so a
//! restart never drops one. Two idempotency guards mean a replayed issue cannot
//! cause a spurious build: processed issues are closed, and an unchanged
//! manifest makes the build a no-op anyway.
//!
//! ```text
//! every cycle:
//!   scan my open `build-ok` issues                 (authoritative catch-up)
//!   → harvest latest-good release of every component → manifest
//!   → manifest == current stormcos release's manifest?  → close issues, done
//!   → changed? build image (devct) → publish stormcos release → QE → close issues
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A resolved component version — one row of the version manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentVersion {
    pub repo: String,
    /// The release tag harvested (the identity that matters).
    pub tag: String,
    /// Whether this is the component's newest release, or a fallback to its
    /// previous one because the newest build failed (best-available harvest).
    #[serde(default)]
    pub fallback: bool,
}

/// The version manifest — what went into a build. This is BOTH the provenance
/// record (builder#1) and the change-detection key: "did the manifest change?"
/// == "should we build?".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// The recipe: the stormcos repo's own resolved ref. stormcos is both recipe
    /// and content — a change to build-base-rootfs.sh / editions / tooling must
    /// rebuild even if no component released.
    pub recipe: ComponentVersion,
    /// Harvested component versions, sorted by repo for a stable identity.
    pub components: Vec<ComponentVersion>,
}

impl Manifest {
    /// Build a manifest from the recipe ref + a repo→tag map. Sorted so identity
    /// is order-independent.
    pub fn new(recipe: ComponentVersion, harvested: BTreeMap<String, ComponentVersion>) -> Self {
        let mut components: Vec<_> = harvested.into_values().collect();
        components.sort_by(|a, b| a.repo.cmp(&b.repo));
        Self { recipe, components }
    }

    /// The identity used for change detection: recipe tag + every component tag.
    /// Fallback status is NOT part of identity — a volume that reached the same
    /// tag via fallback is the same content, so it must not force a rebuild.
    pub fn identity(&self) -> String {
        let mut s = format!("{}={}", self.recipe.repo, self.recipe.tag);
        for c in &self.components {
            s.push(';');
            s.push_str(&c.repo);
            s.push('=');
            s.push_str(&c.tag);
        }
        s
    }

    /// Should we build? True iff the identity differs from the last built one.
    pub fn changed_from(&self, last: Option<&Manifest>) -> bool {
        match last {
            None => true, // never built
            Some(prev) => self.identity() != prev.identity(),
        }
    }
}

/// QE topologies an image is validated on, each with its own test set. A build
/// is not "good" until the topologies its release tier requires have passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Topology {
    /// One node: does it boot, is the runtime up (CRI-O, SELinux enforcing),
    /// does a single-node control plane come Ready.
    Single,
    /// Three control-plane nodes: 3-member etcd, cross-node join, HA.
    ThreeMaster,
    /// Full six-VM cluster: 3 control-plane + 3 workers — scheduling, CNI across
    /// nodes, storage/CSI, real workloads.
    FullSix,
}

impl Topology {
    pub fn node_count(self) -> u32 {
        match self {
            Topology::Single => 1,
            Topology::ThreeMaster => 3,
            Topology::FullSix => 6,
        }
    }

    /// The stormcos_qa test scope name for this topology — the QE manager runs a
    /// different test set per topology (`tests/topology/<name>/`).
    pub fn qa_scope(self) -> &'static str {
        match self {
            Topology::Single => "single",
            Topology::ThreeMaster => "three-master",
            Topology::FullSix => "full-six",
        }
    }

    /// Escalating order — cheapest first. A failure at an earlier topology short-
    /// circuits the rest: no point standing up six VMs if one won't boot.
    pub fn escalation() -> [Topology; 3] {
        [Topology::Single, Topology::ThreeMaster, Topology::FullSix]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cv(repo: &str, tag: &str) -> ComponentVersion {
        ComponentVersion { repo: repo.into(), tag: tag.into(), fallback: false }
    }

    fn manifest(recipe_tag: &str, comps: &[(&str, &str)]) -> Manifest {
        let mut m = BTreeMap::new();
        for (r, t) in comps {
            m.insert(r.to_string(), cv(r, t));
        }
        Manifest::new(cv("glennswest/stormcos", recipe_tag), m)
    }

    #[test]
    fn identity_is_order_independent() {
        let a = manifest("r1", &[("a", "1"), ("b", "2")]);
        let b = manifest("r1", &[("b", "2"), ("a", "1")]);
        assert_eq!(a.identity(), b.identity());
    }

    #[test]
    fn unchanged_manifest_does_not_build() {
        let a = manifest("r1", &[("a", "1"), ("b", "2")]);
        let b = manifest("r1", &[("a", "1"), ("b", "2")]);
        assert!(!a.changed_from(Some(&b)), "identical set must not rebuild");
    }

    #[test]
    fn a_component_bump_builds() {
        let a = manifest("r1", &[("a", "1"), ("b", "2")]);
        let b = manifest("r1", &[("a", "1"), ("b", "3")]); // b bumped
        assert!(b.changed_from(Some(&a)));
    }

    #[test]
    fn a_recipe_change_builds_even_with_no_component_change() {
        // stormcos itself changing (base script/editions/tooling) must rebuild.
        let a = manifest("r1", &[("a", "1")]);
        let b = manifest("r2", &[("a", "1")]); // only the recipe moved
        assert!(b.changed_from(Some(&a)));
    }

    #[test]
    fn fallback_does_not_affect_identity() {
        // Reaching the same tag via a previous-release fallback is the same
        // content — it must not force a rebuild.
        let a = manifest("r1", &[("a", "1")]);
        let mut b = manifest("r1", &[("a", "1")]);
        b.components[0].fallback = true;
        assert!(!b.changed_from(Some(&a)));
    }

    #[test]
    fn first_build_always_runs() {
        assert!(manifest("r1", &[("a", "1")]).changed_from(None));
    }

    #[test]
    fn topology_escalation_is_cheapest_first() {
        let e = Topology::escalation();
        assert_eq!(e[0], Topology::Single);
        assert_eq!(e[2], Topology::FullSix);
        assert!(e[0].node_count() < e[1].node_count());
        assert!(e[1].node_count() < e[2].node_count());
    }

    #[test]
    fn topology_maps_to_a_qa_scope() {
        assert_eq!(Topology::ThreeMaster.qa_scope(), "three-master");
        assert_eq!(Topology::FullSix.node_count(), 6);
    }
}
