//! Clean command.

use crate::{models::FleetConfig, tart};
use anyhow::{Context, Result};
use std::collections::BTreeSet;

/// Delete Tart caches for `cfg`. Include golden images if `goldens` is true.
///
/// # Errors
/// Returns an error if a required host, configuration, or service operation fails.
pub(super) fn run(cfg: &FleetConfig, goldens: bool) -> Result<u8> {
    let images: BTreeSet<&str> = cfg.groups().iter().map(|g| g.image.as_str()).collect();
    let before = fs4::available_space(tart::home()).context("reading free disk space")?;

    // A group can clone its runners directly from an OCI image. For that
    // group, the cache entry is the golden.
    let cached = tart::cached_images()?;
    let oci_goldens: Vec<&str> = images
        .iter()
        .copied()
        .filter(|i| cached.iter().any(|c| c == i))
        .collect();
    if goldens || oci_goldens.is_empty() {
        println!("deleting the tart OCI and IPSW caches...");
        tart::prune_caches()?;
    } else {
        println!(
            "keeping the tart caches: runners clone from {}. Use --goldens to delete them.",
            oci_goldens.join(", ")
        );
    }

    let mut rc = 0;
    if goldens {
        for image in images {
            if !tart::exists(image)? {
                continue;
            }
            match tart::try_delete(image) {
                Ok(()) => println!(
                    "deleted golden {image:?}. Run `bakery build {image}` before the next recycle."
                ),
                Err(e) => {
                    eprintln!("could not delete golden {image:?}: {e}");
                    rc = 1;
                }
            }
        }
    }

    let after = fs4::available_space(tart::home()).context("reading free disk space")?;
    println!(
        "freed {:.1} GB ({:.1} GB free)",
        after.saturating_sub(before) as f64 / 1e9,
        after as f64 / 1e9
    );
    Ok(rc)
}
