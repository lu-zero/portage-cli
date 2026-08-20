//! `em select mirrors` — mirrorselect workalike for managing `GENTOO_MIRRORS`
//!
//! Lists, shows, and sets Gentoo distfile mirrors. The mirror list comes from
//! [`portage_distfiles::MirrorList`] (Gentoo's structured XML API). `GENTOO_MIRRORS`
//! is written to the `make.conf` selected by the global root flags
//! (`--config-root`/`--root`/`--local`/`--prefix`/`--target`) via
//! [`config_portage_dir`].

use anyhow::{Context, Result, bail};
use camino::Utf8PathBuf;

use super::config_portage_dir;
use crate::cli::{Cli, MirrorAction};
use crate::style::C_STAR;
use portage_distfiles::{Mirror, MirrorList};
use portage_repo::MakeConf;

/// Resolve the `make.conf` path the root flags select
fn make_conf_path(globals: &Cli) -> Utf8PathBuf {
    config_portage_dir(globals).join("make.conf")
}

/// Dispatch `em select mirrors <action>`
pub async fn run(action: &MirrorAction, globals: &Cli) -> Result<()> {
    match action {
        MirrorAction::List { country, region } => {
            list_mirrors(globals, country.as_deref(), region.as_deref()).await
        }
        MirrorAction::Show => show(globals),
        MirrorAction::Set {
            urls,
            country,
            region,
        } => set_mirrors(globals, urls, country.as_deref(), region.as_deref()).await,
    }
}

/// List available mirrors, optionally filtered by country or region
async fn list_mirrors(globals: &Cli, country: Option<&str>, region: Option<&str>) -> Result<()> {
    let list = MirrorList::fetch().await;
    let filtered: Vec<&Mirror> = if let Some(c) = country {
        list.by_country(c)
    } else if let Some(r) = region {
        list.by_region(r)
    } else {
        list.all().iter().collect()
    };

    if filtered.is_empty() {
        if country.is_some() || region.is_some() {
            println!("No mirrors match the specified filter.");
        } else {
            println!("No mirrors available.");
        }
        return Ok(());
    }

    // Mark mirrors already present in GENTOO_MIRRORS.
    let current = current_mirror_set(globals);

    println!("Available Gentoo distfile mirrors:\n");
    for (i, mirror) in filtered.iter().enumerate() {
        let Some(endpoint) = mirror.http_endpoint() else {
            continue;
        };
        let marker = if current.contains(endpoint.uri.as_str()) {
            format!(" {C_STAR}*{C_STAR:#}")
        } else {
            String::new()
        };
        println!(
            "  [{i}] {uri} ({country} – {name}){marker}",
            i = i + 1,
            uri = endpoint.uri,
            country = mirror.country,
            name = mirror.name,
        );
    }
    Ok(())
}

/// Show the currently configured `GENTOO_MIRRORS` value
fn show(globals: &Cli) -> Result<()> {
    let path = make_conf_path(globals);
    if !path.exists() {
        println!("(no make.conf at {path})");
        return Ok(());
    }
    let mc = MakeConf::load(&path).with_context(|| format!("reading {path}"))?;
    match mc.get("GENTOO_MIRRORS") {
        Some(value) if !value.is_empty() => println!("{value}"),
        _ => println!("(GENTOO_MIRRORS not set in {path})"),
    }
    Ok(())
}

/// Set `GENTOO_MIRRORS` in `make.conf`
///
/// With explicit URLs those are used verbatim; otherwise every mirror matching
/// `--country`/`--region` is used (one HTTPS-preferred URL per mirror).
async fn set_mirrors(
    globals: &Cli,
    urls: &[String],
    country: Option<&str>,
    region: Option<&str>,
) -> Result<()> {
    let final_urls: Vec<String> = if !urls.is_empty() {
        urls.iter().map(|u| ensure_trailing_slash(u)).collect()
    } else {
        let list = MirrorList::fetch().await;
        let selected: Vec<&Mirror> = match (country, region) {
            (Some(c), _) => list.by_country(c),
            (_, Some(r)) => list.by_region(r),
            _ => bail!("no URLs given and no --country/--region filter; pass URLs or a filter"),
        };
        selected
            .iter()
            .filter_map(|m| m.http_endpoint().map(|e| e.uri.clone()))
            .collect()
    };

    if final_urls.is_empty() {
        bail!("no mirrors matched the specified filter.");
    }

    let value = final_urls.join(" ");
    let path = make_conf_path(globals);

    let mut mc = if path.exists() {
        MakeConf::load(&path).with_context(|| format!("reading {path}"))?
    } else {
        MakeConf::parse(String::new())?
    };
    mc.set("GENTOO_MIRRORS", &value);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating directory {parent}"))?;
    }
    mc.save(&path).with_context(|| format!("writing {path}"))?;

    println!("GENTOO_MIRRORS set to: {value}");
    println!("Written to: {path}");
    Ok(())
}

/// Read the current `GENTOO_MIRRORS` URLs as a set (for the list marker)
fn current_mirror_set(globals: &Cli) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    let path = make_conf_path(globals);
    let Ok(mc) = MakeConf::load(&path) else {
        return set;
    };
    if let Some(value) = mc.get("GENTOO_MIRRORS") {
        for url in value.split_whitespace() {
            set.insert(url.to_string());
        }
    }
    set
}

/// Ensure a mirror URL ends with `/`, matching how portage stores mirror URLs
fn ensure_trailing_slash(url: &str) -> String {
    if url.ends_with('/') {
        url.to_string()
    } else {
        format!("{url}/")
    }
}
