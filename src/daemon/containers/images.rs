//! Root filesystem images from an image server laid out like
//! images.linuxcontainers.org: `meta/1.0/index-system` lists one build
//! directory per distribution, release, architecture and variant, which
//! holds `rootfs.tar.xz` and a `SHA256SUMS` file.

use anyhow::{Result, bail};

use crate::i18n::fl;
use crate::protocol::Image;

pub const INDEX: &str = "meta/1.0/index-system";
pub const ROOTFS: &str = "rootfs.tar.xz";
/// The plain image; "cloud" adds cloud-init.
const VARIANT: &str = "default";

/// This machine's architecture as the image server names it.
pub fn image_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "arm" => "armhf",
        "x86" => "i386",
        "loongarch64" => "loong64",
        "powerpc64" if cfg!(target_endian = "little") => "ppc64el",
        other => other,
    }
}

/// One line of the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub image: Image,
    /// Build directory below the server, e.g. `/images/debian/trixie/amd64/default/20261006_05:24/`.
    pub path: String,
}

/// Default-variant images for `arch`, sorted by distribution and release.
pub fn parse_index(text: &str, arch: &str) -> Vec<IndexEntry> {
    let mut entries: Vec<IndexEntry> = text
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.trim().split(';').collect();
            let [distro, release, entry_arch, variant, build, path] = fields[..] else {
                return None;
            };
            (entry_arch == arch && variant == VARIANT && path.starts_with('/')).then(|| {
                IndexEntry {
                    image: Image {
                        distro: distro.to_owned(),
                        release: release.to_owned(),
                        build: build.to_owned(),
                    },
                    path: path.to_owned(),
                }
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        (a.image.distro.as_str(), a.image.release.as_str())
            .cmp(&(b.image.distro.as_str(), b.image.release.as_str()))
    });
    entries.dedup_by(|a, b| a.image.distro == b.image.distro && a.image.release == b.image.release);
    entries
}

/// Whether `source` names an image (`distro/release`, optionally with an
/// `image:` prefix) rather than a file or URL.
pub fn image_spec(source: &str) -> Option<(String, String)> {
    let spec = source.strip_prefix("image:").unwrap_or(source).trim();
    if spec.starts_with('/') || spec.contains("://") {
        return None;
    }
    let (distro, release) = spec.split_once('/')?;
    let valid = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
    };
    (valid(distro) && valid(release)).then(|| (distro.to_owned(), release.to_owned()))
}

/// The entry for `distro/release` (ASCII case ignored).
pub fn find<'a>(entries: &'a [IndexEntry], distro: &str, release: &str) -> Result<&'a IndexEntry> {
    if let Some(entry) = entries.iter().find(|e| {
        e.image.distro.eq_ignore_ascii_case(distro) && e.image.release.eq_ignore_ascii_case(release)
    }) {
        return Ok(entry);
    }
    let releases: Vec<String> = entries
        .iter()
        .filter(|e| e.image.distro.eq_ignore_ascii_case(distro))
        .map(|e| e.image.spec())
        .collect();
    if releases.is_empty() {
        bail!(fl!(
            "containers-image-unknown",
            image = format!("{distro}/{release}"),
            arch = image_arch()
        ))
    }
    bail!(fl!(
        "containers-image-unknown-release",
        image = format!("{distro}/{release}"),
        releases = crate::util::join_list(&releases)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX_TEXT: &str = "\
debian;trixie;amd64;cloud;20261006_05:24;/images/debian/trixie/amd64/cloud/20261006_05:24/
debian;trixie;amd64;default;20261006_05:24;/images/debian/trixie/amd64/default/20261006_05:24/
debian;bookworm;amd64;default;20261006_05:24;/images/debian/bookworm/amd64/default/20261006_05:24/
alpine;3.22;amd64;default;20261006_13:00;/images/alpine/3.22/amd64/default/20261006_13:00/
alpine;3.22;arm64;default;20261006_13:00;/images/alpine/3.22/arm64/default/20261006_13:00/
broken line
";

    #[test]
    fn index_lists_default_images_for_the_architecture() {
        let entries = parse_index(INDEX_TEXT, "amd64");
        let specs: Vec<String> = entries.iter().map(|e| e.image.spec()).collect();
        assert_eq!(specs, ["alpine/3.22", "debian/bookworm", "debian/trixie"]);
        let trixie = find(&entries, "Debian", "TRIXIE").unwrap();
        assert_eq!(
            trixie.path,
            "/images/debian/trixie/amd64/default/20261006_05:24/"
        );
        let wrong = crate::util::error_chain(&find(&entries, "debian", "sid").unwrap_err());
        assert!(wrong.contains("debian/bookworm") && wrong.contains("debian/trixie"));
        assert!(find(&entries, "gentoo", "current").is_err());
        assert_eq!(parse_index(INDEX_TEXT, "arm64").len(), 1);
    }

    #[test]
    fn sources_that_name_images() {
        assert_eq!(
            image_spec("debian/trixie"),
            Some(("debian".to_owned(), "trixie".to_owned()))
        );
        assert_eq!(
            image_spec("image:alpine/3.22"),
            Some(("alpine".to_owned(), "3.22".to_owned()))
        );
        assert_eq!(image_spec("/srv/rootfs.tar.xz"), None);
        assert_eq!(image_spec("https://example.com/a/b"), None);
        assert_eq!(image_spec("debian"), None);
        assert_eq!(image_spec("a/b/c"), None);
    }
}
