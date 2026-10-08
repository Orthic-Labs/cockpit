//! Descriptor-pinned traversal tests for `platform::children_bounded`.
//!
//! All adversarial fixtures live under the process temp directory; no user
//! volume is ever touched. Assertions are against the production adapter
//! through the public `pulse_core::platform` seam.

use std::path::PathBuf;

struct Fixture(PathBuf);
impl Fixture {
    fn new(name: &str) -> Self {
        let root = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("pulse-race-{name}-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn path(&self) -> &PathBuf {
        &self.0
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
mod unix {
    use super::Fixture;
    use pulse_core::platform::children_bounded;
    use std::fs;
    use std::os::unix::fs::symlink;

    #[test]
    fn swapped_ancestor_symlink_refuses_descendant_listing() {
        let fixture = Fixture::new("ancestor-swap");
        let real = fixture.path().join("real");
        let inner = real.join("inner");
        fs::create_dir_all(&inner).unwrap();
        fs::write(inner.join("entry"), b"x").unwrap();
        let decoy = fixture.path().join("decoy");
        fs::create_dir_all(decoy.join("inner")).unwrap();
        fs::write(decoy.join("inner").join("decoy-entry"), b"x").unwrap();

        // Baseline: the real chain lists fine.
        let (children, truncated) = children_bounded(&inner, 10).unwrap();
        assert_eq!(children, vec![inner.join("entry")]);
        assert!(!truncated);

        // Swap the ancestor `real` for a symlink to `decoy`. The pinned walk
        // opens every component with O_NOFOLLOW, so the link must be refused
        // rather than redirecting the listing to the decoy's contents.
        fs::rename(&real, fixture.path().join("real-away")).unwrap();
        symlink(&decoy, &real).unwrap();
        let error = children_bounded(&inner, 10).unwrap_err();
        assert!(
            error.message.contains("pin") || error.message.contains("link"),
            "unexpected refusal: {}",
            error.message
        );
    }

    #[test]
    fn renamed_replacement_is_listed_as_itself_not_stale_object() {
        let fixture = Fixture::new("replace");
        let victim = fixture.path().join("victim");
        let attacker = fixture.path().join("attacker");
        fs::create_dir(&victim).unwrap();
        fs::create_dir(&attacker).unwrap();
        fs::write(victim.join("original"), b"x").unwrap();
        fs::write(attacker.join("swapped"), b"x").unwrap();

        // Atomically rename a different directory over the enumerated path.
        fs::rename(&victim, fixture.path().join("victim-away")).unwrap();
        fs::rename(&attacker, &victim).unwrap();

        // The adapter must list the object now at the path (verifying the
        // fresh lstat identity against the opened descriptor), never a stale
        // or mixed view of the replaced directory.
        let (children, truncated) = children_bounded(&victim, 10).unwrap();
        assert_eq!(children, vec![victim.join("swapped")]);
        assert!(!truncated);
    }

    #[test]
    fn final_component_symlink_swap_is_refused() {
        let fixture = Fixture::new("leaf-swap");
        let target = fixture.path().join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("entry"), b"x").unwrap();
        let leaf = fixture.path().join("leaf");
        fs::create_dir(&leaf).unwrap();
        fs::remove_dir(&leaf).unwrap();
        symlink(&target, &leaf).unwrap();
        assert!(children_bounded(&leaf, 10).is_err());
    }

    #[test]
    fn bounded_listing_still_reports_truncation() {
        let fixture = Fixture::new("bounds");
        let dir = fixture.path().join("dir");
        fs::create_dir(&dir).unwrap();
        for name in ["a", "b", "c"] {
            fs::write(dir.join(name), b"x").unwrap();
        }
        let (children, truncated) = children_bounded(&dir, 2).unwrap();
        assert_eq!(children.len(), 2);
        assert!(truncated);
        let (all, truncated) = children_bounded(&dir, 10).unwrap();
        assert_eq!(all.len(), 3);
        assert!(!truncated);
    }

    #[test]
    fn listing_survives_parent_rename_after_acceptance() {
        // Renaming a still-ancestor directory does not invalidate an open
        // descriptor chain; a fresh call after the rename resolves the new
        // path and must either list the object now there or refuse — never
        // silently follow a stale name. Here the path is simply gone.
        let fixture = Fixture::new("rename");
        let parent = fixture.path().join("parent");
        let dir = parent.join("dir");
        fs::create_dir_all(&dir).unwrap();
        fs::rename(&parent, fixture.path().join("moved")).unwrap();
        assert!(children_bounded(&dir, 10).is_err());
        let (children, _) = children_bounded(&fixture.path().join("moved/dir"), 10).unwrap();
        assert!(children.is_empty());
    }
}

#[cfg(windows)]
mod windows {
    use super::Fixture;
    use pulse_core::platform::children_bounded;
    use std::fs;
    use std::os::windows::fs::symlink_dir;

    #[test]
    fn junction_ancestor_is_refused() {
        let fixture = Fixture::new("junction");
        let real = fixture.path().join("real");
        let inner = real.join("inner");
        fs::create_dir_all(&inner).unwrap();
        let decoy = fixture.path().join("decoy");
        fs::create_dir_all(decoy.join("inner")).unwrap();
        fs::rename(&real, fixture.path().join("real-away")).unwrap();
        symlink_dir(&decoy, &real).unwrap();
        assert!(children_bounded(&inner, 10).is_err());
    }

    #[test]
    fn bounded_listing_still_reports_truncation() {
        let fixture = Fixture::new("bounds");
        let dir = fixture.path().join("dir");
        fs::create_dir(&dir).unwrap();
        for name in ["a", "b", "c"] {
            fs::write(dir.join(name), b"x").unwrap();
        }
        let (children, truncated) = children_bounded(&dir, 2).unwrap();
        assert_eq!(children.len(), 2);
        assert!(truncated);
    }
}
