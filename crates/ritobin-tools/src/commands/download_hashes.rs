use camino::Utf8PathBuf;
use indicatif::ProgressStyle;
use miette::{IntoDiagnostic, Result, WrapErr};
use std::fs::{self, File};
use std::io::{Read, Write};
use tracing_indicatif::span_ext::IndicatifSpanExt;

use crate::utils::config::load_or_create_config;
use crate::utils::hyperlink_path;

/// Hash files loaded by `load_from_directory` in ltk_ritobin
const HASH_FILES: &[(&str, &str)] = &[
    (
        "hashes.binentries.txt",
        "https://raw.communitydragon.org/binviewer/hashes/hashes.binentries.txt",
    ),
    (
        "hashes.binfields.txt",
        "https://raw.communitydragon.org/binviewer/hashes/hashes.binfields.txt",
    ),
    (
        "hashes.binhashes.txt",
        "https://raw.communitydragon.org/binviewer/hashes/hashes.binhashes.txt",
    ),
    (
        "hashes.bintypes.txt",
        "https://raw.communitydragon.org/binviewer/hashes/hashes.bintypes.txt",
    ),
];

const DOWNLOAD_BUFFER_SIZE: usize = 64 * 1024;

/// Download hashtable files from CommunityDragon to the configured hashtable directory.
pub fn download_hashes() -> Result<()> {
    let (config, _) = load_or_create_config()?;

    let target_dir = config
        .hashtable_dir
        .ok_or_else(|| miette::miette!("No hashtable directory configured"))?;

    fs::create_dir_all(target_dir.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("Failed to create directory: {}", target_dir))?;

    tracing::info!("Downloading hashtables to {}", hyperlink_path(&target_dir));

    for (filename, url) in HASH_FILES {
        download_file_with_progress(url, filename, &target_dir)?;
    }

    tracing::info!(
        "Successfully downloaded all hashtables to {}",
        hyperlink_path(&target_dir)
    );
    Ok(())
}

fn download_file_with_progress(url: &str, filename: &str, target_dir: &Utf8PathBuf) -> Result<()> {
    let response = ureq::get(url)
        .call()
        .map_err(|e| miette::miette!("Failed to download {}: {}", filename, e))?;

    // Get content length for progress bar (if available)
    let content_length: Option<u64> = response
        .header("Content-Length")
        .and_then(|s| s.parse().ok());

    let target_path = target_dir.join(filename);
    let mut file = File::create(target_path.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("Failed to create file: {}", target_path))?;

    let mut reader = response.into_reader();
    let mut buffer = [0u8; DOWNLOAD_BUFFER_SIZE];
    let mut downloaded: u64 = 0;

    // Create a tracing span for the progress bar
    let span = tracing::info_span!("download", file = %filename);
    let _entered = span.enter();

    if let Some(total) = content_length {
        span.pb_set_style(
            &ProgressStyle::with_template(
                "{msg} {wide_bar:40.cyan/blue} {bytes}/{total_bytes} ({bytes_per_sec})",
            )
            .unwrap(),
        );
        span.pb_set_length(total);
    } else {
        span.pb_set_style(
            &ProgressStyle::with_template("{msg} {bytes} downloaded ({bytes_per_sec})").unwrap(),
        );
    }
    span.pb_set_message(filename);

    loop {
        let bytes_read = reader
            .read(&mut buffer)
            .into_diagnostic()
            .wrap_err("Failed to read from download stream")?;
        if bytes_read == 0 {
            break;
        }

        file.write_all(&buffer[..bytes_read])
            .into_diagnostic()
            .wrap_err("Failed to write to file")?;
        downloaded += bytes_read as u64;
        span.pb_set_position(downloaded);
    }

    tracing::info!(
        "Saved {} ({} bytes)",
        hyperlink_path(&target_path),
        downloaded
    );
    Ok(())
}
