//! Plan command.

use crate::{
    host,
    models::{runner_names, FleetConfig},
};
use anyhow::Result;

/// Print resource use for `cfg` and return 1 if the fleet exceeds the host budget.
///
/// # Errors
/// Returns an error if a required host, configuration, or service operation fails.
pub(super) fn run(cfg: &FleetConfig) -> Result<u8> {
    let hr = host::detect()?;
    let reserve = cfg.reserve();
    let overcommit = host::DEFAULT_OVERCOMMIT;
    let b = host::budget(hr, reserve, overcommit);
    println!("Host:    {} cores, {} GB RAM", hr.cpu, hr.memory_gb);
    println!(
        "Reserve: {} cores, {} GB RAM",
        reserve.cpu, reserve.memory_gb
    );
    println!(
        "Budget:  {} cores, {} GB RAM ({:.2}x over-commit)",
        b.cpu, b.memory_gb, overcommit,
    );
    println!(
        "Org:     {} (token from ${})\n",
        cfg.github.org, cfg.github.token_env
    );

    println!("Planned runners:");
    let (mut total_cpu, mut total_mem) = (0u32, 0u32);
    for g in cfg.groups() {
        for name in runner_names(g) {
            total_cpu += g.resources.cpu;
            total_mem += g.resources.memory_gb;
            let disk = match g.resources.disk_floor() {
                Some(d) => d.to_string(),
                None => "-".into(),
            };
            println!(
                "  - {name:<22} kind={kind:<5} image={image:<16} cpu={cpu} ram={ram}G disk={disk}G ephemeral={ephemeral} labels={labels}",
                ephemeral = g.ephemeral,
                kind = g.kind.as_str(),
                image = g.image,
                cpu = g.resources.cpu,
                ram = g.resources.memory_gb,
                labels = g.labels.join(","),
            );
        }
    }
    println!("\nTotal:   cpu={total_cpu} ram={total_mem}G");

    let problems = host::check_fit(hr, reserve, cfg.groups(), overcommit);
    if problems.is_empty() {
        println!("\nFleet fits within host budget.");
        return Ok(0);
    }
    println!("\nFleet does NOT fit:");
    for p in &problems {
        println!("  ! {p}");
    }
    Ok(1)
}
