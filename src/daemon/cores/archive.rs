//! Recognising sing-box release assets and unpacking them.
//!
//! Forks name their archives `sing-box-<version>-linux-<arch>[-<variant>].<ext>`
//! with slightly different architecture spellings (`armv7` vs `arm-v7`) and
//! variants (`glibc`, `musl`, `ebpf`, `v3-glibc`, ...).

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::daemon::github::Asset;
use crate::i18n::fl;
use crate::util::fmt_bytes;

const MAX_UNPACKED_BYTES: u64 = 512 * 1024 * 1024;
const ARCHIVE_SUFFIXES: [&str; 4] = [".tar.gz", ".tgz", ".zip", ".gz"];

/// Architecture spellings used in release assets for this machine.
pub fn arch_tokens(configured: Option<&str>) -> Vec<String> {
    if let Some(arch) = configured.filter(|a| !a.is_empty()) {
        return vec![arch.to_owned()];
    }
    let tokens: &[&str] = match std::env::consts::ARCH {
        "x86_64" => &["amd64"],
        "aarch64" => &["arm64"],
        "arm" => &["armv7", "arm-v7"],
        "x86" => &["386"],
        "riscv64" => &["riscv64"],
        "loongarch64" => &["loong64"],
        "mips" if cfg!(target_endian = "little") => &["mipsle"],
        "mips64" if cfg!(target_endian = "little") => &["mips64le"],
        "powerpc64" if cfg!(target_endian = "little") => &["ppc64le"],
        "s390x" => &["s390x"],
        other => return vec![other.to_owned()],
    };
    tokens.iter().map(|t| (*t).to_owned()).collect()
}

/// `(variant, asset)` for every core build of this release for `tokens`;
/// the plain build has the variant "" and comes first.
pub fn match_assets<'a>(assets: &'a [Asset], tokens: &[String]) -> Vec<(String, &'a Asset)> {
    let mut found: Vec<(String, &Asset)> = Vec::new();
    for asset in assets {
        let Some(stem) = asset.name.strip_prefix("sing-box-") else {
            continue;
        };
        let Some(stem) = ARCHIVE_SUFFIXES.iter().find_map(|s| stem.strip_suffix(s)) else {
            continue;
        };
        let Some((_, platform)) = stem.split_once("-linux-") else {
            continue;
        };
        let variant = tokens.iter().find_map(|token| {
            if platform == token {
                Some(String::new())
            } else {
                platform
                    .strip_prefix(token.as_str())
                    .and_then(|rest| rest.strip_prefix('-'))
                    .map(str::to_owned)
            }
        });
        if let Some(variant) = variant
            && !found.iter().any(|(v, _)| *v == variant)
        {
            found.push((variant, asset));
        }
    }
    found.sort_by(|(a, _), (b, _)| (!a.is_empty(), a).cmp(&(!b.is_empty(), b)));
    found
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    TarGz,
    Zip,
    Gzip,
    Raw,
}

fn detect(name: &str, data: &[u8]) -> Format {
    let name = name.to_ascii_lowercase();
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        return Format::TarGz;
    }
    if name.ends_with(".zip") || data.starts_with(b"PK\x03\x04") {
        return Format::Zip;
    }
    if data.starts_with(&[0x1f, 0x8b]) {
        // A gzip stream: a tarball if the decompressed data has a tar header.
        let mut head = vec![0u8; 512];
        let mut decoder = flate2::read::GzDecoder::new(data);
        if decoder.read_exact(&mut head).is_ok() && &head[257..262] == b"ustar" {
            return Format::TarGz;
        }
        return Format::Gzip;
    }
    Format::Raw
}

/// Unpacks a core into the empty directory `dir`: every regular file of the
/// archive is placed directly in `dir` (directories are flattened), and the
/// executable is named `sing-box`. Returns the unpacked file names.
pub fn unpack(name: &str, data: &[u8], dir: &Path) -> Result<Vec<String>> {
    std::fs::create_dir_all(dir)?;
    let mut files: Vec<(String, bool)> = Vec::new();
    let mut total = 0u64;
    let mut write = |file_name: &str, reader: &mut dyn Read, executable: bool| -> Result<()> {
        if file_name.is_empty()
            || file_name.starts_with('.')
            || files.iter().any(|(n, _)| n == file_name)
        {
            return Ok(());
        }
        let path = dir.join(file_name);
        let mut out = std::fs::File::create(&path)
            .with_context(|| fl!("err-create", path = path.display().to_string()))?;
        let copied = std::io::copy(&mut reader.take(MAX_UNPACKED_BYTES - total + 1), &mut out)?;
        total += copied;
        if total > MAX_UNPACKED_BYTES {
            bail!(fl!(
                "archive-too-large",
                name = name,
                limit = fmt_bytes(MAX_UNPACKED_BYTES)
            ));
        }
        let mode = if executable { 0o755 } else { 0o644 };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))?;
        files.push((file_name.to_owned(), executable));
        Ok(())
    };

    match detect(name, data) {
        Format::TarGz => {
            let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(data));
            for entry in archive.entries().context(fl!("archive-read-failed"))? {
                let mut entry = entry?;
                if !entry.header().entry_type().is_file() {
                    continue;
                }
                let path = entry.path()?.into_owned();
                let file_name = base_name(&path);
                let executable = entry.header().mode().unwrap_or(0) & 0o111 != 0;
                write(&file_name, &mut entry, executable)?;
            }
        }
        Format::Zip => {
            let mut zip =
                zip::ZipArchive::new(std::io::Cursor::new(data)).context(fl!("err-open-zip"))?;
            for i in 0..zip.len() {
                let mut file = zip.by_index(i)?;
                if file.is_dir() {
                    continue;
                }
                let Some(path) = file.enclosed_name() else {
                    continue;
                };
                let executable = file.unix_mode().is_some_and(|m| m & 0o111 != 0);
                write(&base_name(&path), &mut file, executable)?;
            }
        }
        Format::Gzip => {
            write("sing-box", &mut flate2::read::GzDecoder::new(data), true)?;
        }
        Format::Raw => {
            write("sing-box", &mut std::io::Cursor::new(data), true)?;
        }
    }

    // The core: `sing-box` itself, else the first `sing-box*` executable.
    let main = files
        .iter()
        .find(|(n, _)| n == "sing-box")
        .or_else(|| {
            files
                .iter()
                .find(|(n, exec)| n.starts_with("sing-box") && *exec)
        })
        .or_else(|| files.iter().find(|(n, _)| is_elf(&dir.join(n))))
        .map(|(n, _)| n.clone());
    let Some(main) = main else {
        bail!(fl!("archive-no-binary", name = name));
    };
    if main != "sing-box" {
        std::fs::rename(dir.join(&main), dir.join("sing-box"))?;
    }
    std::fs::set_permissions(dir.join("sing-box"), std::fs::Permissions::from_mode(0o755))?;
    let mut names: Vec<String> = files
        .into_iter()
        .map(|(n, _)| if n == main { "sing-box".to_owned() } else { n })
        .collect();
    names.sort();
    Ok(names)
}

fn base_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_owned()
}

fn is_elf(path: &PathBuf) -> bool {
    let mut magic = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .is_ok()
        && magic == *b"\x7fELF"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str) -> Asset {
        Asset {
            name: name.to_owned(),
            browser_download_url: format!("https://example.com/{name}"),
            digest: None,
            size: 1,
        }
    }

    fn variants(names: &[&str], tokens: &[&str]) -> Vec<String> {
        let assets: Vec<Asset> = names.iter().map(|n| asset(n)).collect();
        let tokens: Vec<String> = tokens.iter().map(|t| (*t).to_owned()).collect();
        match_assets(&assets, &tokens)
            .into_iter()
            .map(|(v, a)| format!("{v}={}", a.name))
            .collect()
    }

    #[test]
    fn sagernet_assets() {
        let names = [
            "sing-box-1.15.0-alpha.10-linux-amd64.tar.gz",
            "sing-box-1.15.0-alpha.10-linux-amd64-glibc.tar.gz",
            "sing-box-1.15.0-alpha.10-linux-amd64-musl.tar.gz",
            "sing-box-1.15.0-alpha.10-linux-arm64.tar.gz",
            "sing-box-1.15.0-alpha.10-windows-amd64.zip",
            "SFL-1.15.0-alpha.10-amd64.deb",
        ];
        assert_eq!(
            variants(&names, &["amd64"]),
            [
                "=sing-box-1.15.0-alpha.10-linux-amd64.tar.gz",
                "glibc=sing-box-1.15.0-alpha.10-linux-amd64-glibc.tar.gz",
                "musl=sing-box-1.15.0-alpha.10-linux-amd64-musl.tar.gz",
            ]
        );
    }

    #[test]
    fn michongs_assets() {
        let names = [
            "sing-box-1.14.1-xiaobaf14g.1-linux-amd64-ebpf.tar.gz",
            "sing-box-1.14.1-xiaobaf14g.1-linux-amd64.tar.gz",
            "sing-box-1.14.1-xiaobaf14g.1-linux-amd64-v3-glibc-ebpf.tar.gz",
            "sing-box-1.14.1-xiaobaf14g.1-linux-arm-v7.tar.gz",
            "sing-box-1.14.1-xiaobaf14g.1-linux-arm-v7-ebpf.tar.gz",
            "SHA256SUMS",
        ];
        assert_eq!(
            variants(&names, &["amd64"]),
            [
                "=sing-box-1.14.1-xiaobaf14g.1-linux-amd64.tar.gz",
                "ebpf=sing-box-1.14.1-xiaobaf14g.1-linux-amd64-ebpf.tar.gz",
                "v3-glibc-ebpf=sing-box-1.14.1-xiaobaf14g.1-linux-amd64-v3-glibc-ebpf.tar.gz",
            ]
        );
        assert_eq!(
            variants(&names, &["armv7", "arm-v7"]),
            [
                "=sing-box-1.14.1-xiaobaf14g.1-linux-arm-v7.tar.gz",
                "ebpf=sing-box-1.14.1-xiaobaf14g.1-linux-arm-v7-ebpf.tar.gz",
            ]
        );
    }

    fn targz(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (path, content, mode) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(*mode);
            header.set_cksum();
            builder.append_data(&mut header, path, *content).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sbb-unpack-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn unpack_keeps_companion_libraries() {
        let data = targz(&[
            ("sing-box-1.15.0-linux-amd64/libcronet.so", b"lib", 0o644),
            ("sing-box-1.15.0-linux-amd64/LICENSE", b"license", 0o644),
            ("sing-box-1.15.0-linux-amd64/sing-box", b"\x7fELFbin", 0o755),
        ]);
        let dir = temp("tar");
        let files = unpack("sing-box-1.15.0-linux-amd64.tar.gz", &data, &dir).unwrap();
        assert_eq!(files, ["LICENSE", "libcronet.so", "sing-box"]);
        assert_eq!(std::fs::read(dir.join("libcronet.so")).unwrap(), b"lib");
        let mode = std::fs::metadata(dir.join("sing-box"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unpack_renames_versioned_binary_and_raw_files() {
        let data = targz(&[(
            "sing-box-1.14.1-xiaobaf14g.1-linux-amd64",
            b"\x7fELF",
            0o755,
        )]);
        let dir = temp("renamed");
        assert_eq!(unpack("x.tar.gz", &data, &dir).unwrap(), ["sing-box"]);
        std::fs::remove_dir_all(&dir).unwrap();

        let dir = temp("raw");
        assert_eq!(
            unpack("my-build", b"\x7fELF raw", &dir).unwrap(),
            ["sing-box"]
        );
        assert_eq!(std::fs::read(dir.join("sing-box")).unwrap(), b"\x7fELF raw");
        std::fs::remove_dir_all(&dir).unwrap();

        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, b"\x7fELF gz").unwrap();
        let dir = temp("gz");
        assert_eq!(
            unpack("core.gz", &gz.finish().unwrap(), &dir).unwrap(),
            ["sing-box"]
        );
        assert_eq!(std::fs::read(dir.join("sing-box")).unwrap(), b"\x7fELF gz");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unpack_rejects_archives_without_core() {
        let data = targz(&[("readme.txt", b"hi", 0o644)]);
        let dir = temp("none");
        assert!(unpack("x.tar.gz", &data, &dir).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
