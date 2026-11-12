//! The installer's `.sha256` file, as `xtask installer` writes it
//! (`sha256sum` format), and the digest of what was downloaded.

use std::io::{self, Read, Write};

use sha2::{Digest, Sha256};

/// The most bytes of a checksum file read.
pub(crate) const CHECKSUM_LIMIT: u64 = 4096;

/// Why a checksum file was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ChecksumError {
    #[error("the checksum file is not a SHA-256 line")]
    Malformed,
    #[error("the checksum file names {0}, not the installer")]
    OtherFile(String),
}

/// The lower-case hex digest the file gives for `name`: one line, the 64
/// hex digits, two spaces (or a space and `*`), then exactly `name`.
pub(crate) fn expected_digest(text: &str, name: &str) -> Result<String, ChecksumError> {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .ok_or(ChecksumError::Malformed)?;
    let line = line.trim_start_matches('\u{feff}').trim_end();
    let (digest, rest) = line.split_once(' ').ok_or(ChecksumError::Malformed)?;
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ChecksumError::Malformed);
    }
    let file = rest
        .strip_prefix(' ')
        .or_else(|| rest.strip_prefix('*'))
        .ok_or(ChecksumError::Malformed)?;
    if file != name {
        return Err(ChecksumError::OtherFile(file.to_owned()));
    }
    Ok(digest.to_ascii_lowercase())
}

/// A sink that hashes what passes through it.
pub(crate) struct Hashing<W> {
    inner: W,
    hasher: Sha256,
}

impl<W: Write> Hashing<W> {
    pub(crate) fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
        }
    }

    /// The lower-case hex digest of everything written, and the sink.
    pub(crate) fn finish(self) -> (String, W) {
        (hex(&self.hasher.finalize()), self.inner)
    }
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// The lower-case hex SHA-256 of what `file` reads from here on.
pub(crate) fn reader_digest(file: &mut impl Read) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            return Ok(hex(&hasher.finalize()));
        }
        hasher.update(&buffer[..n]);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAME: &str = "yien-0.1.0-x86_64-setup.exe";
    /// SHA-256 of "abc" (FIPS 180-2 test vector).
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn the_sidecar_xtask_writes_is_read_back() {
        // The exact format of `xtask installer`'s `sha256_sidecar`.
        let text = format!("{ABC}  {NAME}\n");
        assert_eq!(expected_digest(&text, NAME), Ok(ABC.to_owned()));
        let upper = format!("{}  {NAME}\r\n", ABC.to_ascii_uppercase());
        assert_eq!(expected_digest(&upper, NAME), Ok(ABC.to_owned()));
        let binary = format!("{ABC} *{NAME}");
        assert_eq!(expected_digest(&binary, NAME), Ok(ABC.to_owned()));
        let bom = format!("\u{feff}{ABC}  {NAME}\n");
        assert_eq!(expected_digest(&bom, NAME), Ok(ABC.to_owned()));
    }

    #[test]
    fn a_malformed_or_foreign_sidecar_is_refused() {
        for bad in [
            String::new(),
            "\n\n".to_owned(),
            format!("{}  {NAME}", &ABC[..63]),
            format!("{}g  {NAME}", &ABC[..63]),
            format!("{ABC}{NAME}"),
            format!("{ABC}\t{NAME}"),
            "<html>Not found</html>".to_owned(),
        ] {
            assert_eq!(
                expected_digest(&bad, NAME),
                Err(ChecksumError::Malformed),
                "{bad:?}"
            );
        }
        assert_eq!(
            expected_digest(&format!("{ABC}  yien-0.0.9-x86_64-setup.exe"), NAME),
            Err(ChecksumError::OtherFile(
                "yien-0.0.9-x86_64-setup.exe".into()
            ))
        );
        assert!(matches!(
            expected_digest(&format!("{ABC}  ../{NAME}"), NAME),
            Err(ChecksumError::OtherFile(_))
        ));
    }

    #[test]
    fn the_digest_of_what_was_written_and_of_a_file_agree() {
        let mut sink = Hashing::new(Vec::new());
        sink.write_all(b"a").unwrap();
        sink.write_all(b"bc").unwrap();
        let (digest, bytes) = sink.finish();
        assert_eq!(digest, ABC);
        assert_eq!(bytes, b"abc");
        let dir = std::env::temp_dir().join(format!("yien-checksum-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("abc.bin");
        std::fs::write(&file, b"abc").unwrap();
        assert_eq!(
            reader_digest(&mut std::fs::File::open(&file).unwrap()).unwrap(),
            ABC
        );
        std::fs::write(&file, b"abd").unwrap();
        assert_ne!(
            reader_digest(&mut std::fs::File::open(&file).unwrap()).unwrap(),
            ABC,
            "a changed byte changes it"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
