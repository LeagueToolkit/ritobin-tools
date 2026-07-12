use itertools::Itertools;
use ltk_mimir_cache::UpdateOptions;
use miette::{IntoDiagnostic, Result};
use tracing::{info, warn};

use crate::Context;

pub fn download_hashes(ctx: &Context) -> Result<()> {
    let Some(hashes) = &ctx.hash_store else {
        return Ok(());
    };

    info!("Hash manifest path: {:?}", hashes.manifest_path());
    info!("Downloading latest hashes from 'https://github.com/LeagueToolkit/mimir/releases'...");

    let fetch = |filename: &str| -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
        let url =
            format!("https://github.com/LeagueToolkit/mimir/releases/latest/download/{filename}");
        Ok(reqwest::blocking::get(&url)?
            .error_for_status()?
            .bytes()?
            .to_vec())
    };
    match hashes
        .update(&fetch, UpdateOptions::default())
        .into_diagnostic()?
    {
        ltk_mimir_cache::UpdateOutcome::Locked => {
            info!("Someone else is already updating hashtables - doing nothing.")
        }
        ltk_mimir_cache::UpdateOutcome::Completed(update_report) => {
            if update_report.installed.is_empty() {
                info!("Everything up to date.");
            } else {
                info!(
                    "Updated {} tables:\n - {}",
                    update_report.installed.len(),
                    update_report.installed.iter().map(|i| i.id()).join(", ")
                );
            }
            if !update_report.unknown_tables.is_empty() {
                warn!(
                    "Found {} tables we don't recognise (you should probably update ritobin-tools):\n - {}",
                    update_report.unknown_tables.len(),
                    update_report.unknown_tables.join(", ")
                );
            }
        }
    }

    Ok(())
}
