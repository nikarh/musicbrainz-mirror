//! MusicBrainz's full exports: which one is the newest (`LATEST`), and their archives, downloaded with resume and
//! checked against the export's `SHA256SUMS`.

use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest as _, Sha256};

/// The archives a database is built from.
pub const ARCHIVES: &[&str] = &[
    "mbdump.tar.bz2",
    "mbdump-derived.tar.bz2",
    "mbdump-cover-art-archive.tar.bz2",
];

/// A download that delivers nothing for this long is abandoned (and resumed by the next attempt).
const STALL_TIMEOUT: Duration = Duration::from_secs(300);
const PROGRESS_EVERY: u64 = 1 << 30;

#[derive(Debug, thiserror::Error)]
pub enum DumpError {
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("the download stalled for {}s", STALL_TIMEOUT.as_secs())]
    Stalled,
    #[error("unexpected LATEST {0:?}")]
    BadLatest(String),
    #[error("{0} is not in the export's SHA256SUMS (not fully published yet?)")]
    NoChecksum(String),
    #[error("{0}: checksum mismatch")]
    Checksum(String),
}

/// A dump's name: its directory, `YYYYMMDD-HHMMSS`. Only names of this shape are accepted (they become file names,
/// and they sort by date).
#[must_use]
pub fn is_dump_id(id: &str) -> bool {
    let b = id.as_bytes();
    b.len() == 15
        && b.iter().enumerate().all(|(i, c)| {
            if i == 8 {
                *c == b'-'
            } else {
                c.is_ascii_digit()
            }
        })
}

/// The newest export.
pub async fn latest(client: &reqwest::Client, dump_url: &str) -> Result<String, DumpError> {
    let text = client
        .get(format!("{dump_url}/LATEST"))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let id = text.trim().to_owned();
    if is_dump_id(&id) {
        Ok(id)
    } else {
        Err(DumpError::BadLatest(id))
    }
}

/// The export's checksums of the archives: file name → hex SHA-256.
pub async fn checksums(
    client: &reqwest::Client,
    dump_url: &str,
    id: &str,
) -> Result<Vec<(String, String)>, DumpError> {
    let text = client
        .get(format!("{dump_url}/{id}/SHA256SUMS"))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    Ok(parse_checksums(&text))
}

fn parse_checksums(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (hash, name) = line.split_once(char::is_whitespace)?;
            Some((
                name.trim().trim_start_matches('*').to_owned(),
                hash.to_lowercase(),
            ))
        })
        .collect()
}

/// Downloads `url` to `dest` unless a file there already has the checksum. An interrupted download is resumed (a
/// `.partial` file next to `dest`).
pub async fn fetch(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    sha256: &str,
) -> Result<(), DumpError> {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if dest.exists() {
        if hash_file(dest).await? == sha256 {
            tracing::info!(file = name, "already downloaded (checksum verified)");
            return Ok(());
        }
        let _ = tokio::fs::remove_file(dest).await;
    }
    let partial = crate::store::file::with_ext(dest, "partial");
    let have = tokio::fs::metadata(&partial).await.map_or(0, |m| m.len());
    let mut request = client.get(url);
    if have > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let began = std::time::Instant::now();
    let mut response = request.send().await?.error_for_status()?;
    let resumed = have > 0 && response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    let total = response
        .content_length()
        .map(|l| l + if resumed { have } else { 0 });
    tracing::info!(
        file = name,
        resumed_at_mb = if resumed { have >> 20 } else { 0 },
        total_mb = total.map(|t| t >> 20),
        "downloading"
    );

    let io = |e| DumpError::Io(partial.clone(), e);
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resumed)
        .truncate(!resumed)
        .open(&partial)
        .await
        .map_err(io)?;
    let mut received = if resumed { have } else { 0 };
    let mut next_report = received + PROGRESS_EVERY;
    loop {
        let Some(chunk) = tokio::time::timeout(STALL_TIMEOUT, response.chunk())
            .await
            .map_err(|_| DumpError::Stalled)??
        else {
            break;
        };
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
            .await
            .map_err(io)?;
        received += chunk.len() as u64;
        if received >= next_report {
            tracing::info!(
                file = name,
                downloaded_mb = received >> 20,
                total_mb = total.map(|t| t >> 20),
                "downloading"
            );
            next_report += PROGRESS_EVERY;
        }
    }
    tokio::io::AsyncWriteExt::flush(&mut file)
        .await
        .map_err(io)?;
    file.sync_all().await.map_err(io)?;
    drop(file);

    if hash_file(&partial).await? != sha256 {
        // Corrupt (or from a changed file): start over next time.
        let _ = tokio::fs::remove_file(&partial).await;
        return Err(DumpError::Checksum(name));
    }
    tokio::fs::rename(&partial, dest)
        .await
        .map_err(|e| DumpError::Io(dest.to_owned(), e))?;
    tracing::info!(
        file = name,
        size_mb = received >> 20,
        elapsed_s = began.elapsed().as_secs(),
        "downloaded (checksum verified)"
    );
    Ok(())
}

async fn hash_file(path: &Path) -> Result<String, DumpError> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let io = |e| DumpError::Io(path.clone(), e);
        let mut file = std::fs::File::open(&path).map_err(io)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0; 1 << 20];
        loop {
            let n = file.read(&mut buf).map_err(io)?;
            if n == 0 {
                break;
            }
            hasher.update(buf.get(..n).unwrap_or_default());
        }
        let mut hex = String::with_capacity(64);
        for byte in hasher.finalize() {
            let _ = write!(hex, "{byte:02x}");
        }
        Ok(hex)
    })
    .await
    .map_err(|e| DumpError::Io(PathBuf::new(), std::io::Error::other(e)))?
}

/// Downloads a dump's archives into `dir`.
pub async fn download(
    client: &reqwest::Client,
    dump_url: &str,
    id: &str,
    dir: &Path,
) -> Result<Vec<PathBuf>, DumpError> {
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| DumpError::Io(dir.to_owned(), e))?;
    let sums = checksums(client, dump_url, id).await?;
    let mut paths = Vec::with_capacity(ARCHIVES.len());
    for archive in ARCHIVES {
        let sha256 = sums
            .iter()
            .find(|(name, _)| name == archive)
            .map(|(_, h)| h.clone())
            .ok_or_else(|| DumpError::NoChecksum((*archive).to_owned()))?;
        let dest = dir.join(archive);
        fetch(
            client,
            &format!("{dump_url}/{id}/{archive}"),
            &dest,
            &sha256,
        )
        .await?;
        paths.push(dest);
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dump_ids() {
        assert!(is_dump_id("20261104-002222"));
        assert!(!is_dump_id("20261104-00222"));
        assert!(!is_dump_id("../../etc/passw"));
        assert!(!is_dump_id("2026110400-2222"));
    }

    #[test]
    fn checksum_files() {
        let sums = parse_checksums("ABC *mbdump.tar.bz2\ndef  mbdump-derived.tar.bz2\n\n");
        assert_eq!(
            sums,
            vec![
                ("mbdump.tar.bz2".into(), "abc".into()),
                ("mbdump-derived.tar.bz2".into(), "def".into())
            ]
        );
    }
}
