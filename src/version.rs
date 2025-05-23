use std::cmp::Ordering;

use crate::Result;
use eyre::bail;
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn version_cmp(version: &str) -> Result<Ordering> {
    let version = semver::Version::parse(version)?;
    let current = semver::Version::parse(env!("CARGO_PKG_VERSION"))?;
    Ok(version.cmp(&current))
}

pub fn version_cmp_or_bail(v: &str) -> Result<()> {
    match version_cmp(v) {
        Ok(Ordering::Greater) => {
            bail!(
                "hk version {v} is less than the minimum required version {}",
                version()
            );
        }
        _ => Ok(()),
    }
}
