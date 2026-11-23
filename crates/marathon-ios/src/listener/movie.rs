//! U267: a received movie reaches the report only when a player can open it.
//!
//! simctl can exit cleanly after INT on a recording a few hundred milliseconds long and still leave an empty or
//! unfinished movie. A transcode of it fails, and the fallback pulled that original and published it as is. The movie
//! is therefore received into a private directory beside its report path, checked there, and renamed into place
//! only if it passed, so a failed check leaves no file and any earlier file at that path untouched.
use std::{
    fs,
    io::{self, BufReader, Read, Seek, SeekFrom},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

/// Why a received movie was not published.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unplayable {
    Empty,
    /// No top-level `moov` box: the sample tables a player needs are missing, as when the writer never finalized.
    NoMovieBox,
}
impl Unplayable {
    pub fn reason(self) -> &'static str {
        match self {
            Self::Empty => "the recording is empty",
            Self::NoMovieBox => "the recording is not a playable movie (it has no moov box)",
        }
    }
}

/// Checks an MP4 or QuickTime file by its top-level boxes. This reads a few headers and none of the media, so it costs
/// no worker command and needs no ffmpeg. Every MP4 and QuickTime writer, fragmented ones included, writes a top-level
/// `moov`; without it no player can open the file.
pub fn check(path: &Path) -> io::Result<Result<(), Unplayable>> {
    let file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(Err(Unplayable::Empty));
    }
    let mut file = BufReader::new(file);
    let mut offset = 0u64;
    // Each box is at least its 8-byte header long, so the walk ends.
    while len - offset >= 8 {
        let mut header = [0u8; 8];
        file.read_exact(&mut header)?;
        let size = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
        let (size, read) = match size {
            0 => (len - offset, 8),
            1 => {
                if len - offset < 16 {
                    break;
                }
                let mut large = [0u8; 8];
                file.read_exact(&mut large)?;
                (u64::from_be_bytes(large), 16)
            }
            size => (u64::from(size), 8),
        };
        if size < read || size > len - offset {
            break;
        }
        if &header[4..] == b"moov" {
            return Ok(Ok(()));
        }
        offset += size;
        file.seek(SeekFrom::Start(offset))?;
    }
    Ok(Err(Unplayable::NoMovieBox))
}

/// A movie's private receiving directory, created beside its report path under the D97 staging prefix. It is a
/// directory, not a sibling file, so the receive keeps the report file's name (the transfer log names it).
pub struct MovieStage {
    root: tempfile::TempDir,
    content: PathBuf,
    destination: PathBuf,
}
impl MovieStage {
    /// Blocking: creates the directory.
    pub fn new(destination: PathBuf) -> io::Result<Self> {
        let name = destination
            .file_name()
            .ok_or_else(|| io::Error::other(format!("{} has no file name", destination.display())))?;
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let root = tempfile::Builder::new()
            .prefix(".marathon-artifact-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(parent)?;
        let content = root.path().join(name);
        Ok(Self {
            root,
            content,
            destination,
        })
    }
    /// Where the receive writes.
    pub fn content(&self) -> &Path {
        &self.content
    }
    /// Blocking: renames a playable movie to its report path, then removes the directory either way.
    pub fn publish(self) -> io::Result<Result<PathBuf, Unplayable>> {
        let checked = check(&self.content)?;
        if checked.is_ok() {
            fs::rename(&self.content, &self.destination)?;
        }
        self.root.close()?;
        Ok(checked.map(|()| self.destination))
    }
    /// Blocking: removes the directory and anything received into it.
    pub fn discard(self) {
        let _ = self.root.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORDING: &[u8] = include_bytes!("../../../../fixtures/fake-worker/apple/media/recording.mp4");
    const TRANSCODED: &[u8] = include_bytes!("../../../../fixtures/fake-worker/apple/media/transcoded.mp4");

    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = u32::try_from(body.len() + 8).unwrap().to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }
    fn checked(bytes: &[u8]) -> Result<(), Unplayable> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("movie.mp4");
        fs::write(&path, bytes).unwrap();
        check(&path).unwrap()
    }

    #[test]
    fn movies_with_a_moov_box_anywhere_at_the_top_level_are_playable() {
        // simctl's layout (moov last) and ffmpeg's faststart layout (moov first).
        assert_eq!(checked(RECORDING), Ok(()));
        assert_eq!(checked(TRANSCODED), Ok(()));
        let ftyp = boxed(b"ftyp", b"isom\0\0\x02\0isom");
        // A 64-bit mdat, then moov.
        let mut large = ftyp.clone();
        large.extend_from_slice(&1u32.to_be_bytes());
        large.extend_from_slice(b"mdat");
        large.extend_from_slice(&(16u64 + 4).to_be_bytes());
        large.extend_from_slice(b"data");
        large.extend(boxed(b"moov", b"mvhd"));
        assert_eq!(checked(&large), Ok(()));
        // A last box whose size is 0 runs to the end of the file.
        let mut open = ftyp.clone();
        open.extend(boxed(b"moov", b"mvhd"));
        open.extend_from_slice(&0u32.to_be_bytes());
        open.extend_from_slice(b"mdat");
        open.extend_from_slice(b"data");
        assert_eq!(checked(&open), Ok(()));
    }

    #[test]
    fn empty_unfinished_and_foreign_files_are_unplayable() {
        assert_eq!(checked(b""), Err(Unplayable::Empty));
        // An unfinalized recording: everything up to the media data, no moov.
        let moov_at = RECORDING.windows(4).position(|w| w == b"moov").unwrap() - 4;
        assert_eq!(checked(&RECORDING[..moov_at]), Err(Unplayable::NoMovieBox));
        // A moov box cut short is not a moov box.
        assert_eq!(checked(&RECORDING[..RECORDING.len() - 1]), Err(Unplayable::NoMovieBox));
        assert_eq!(checked(b"media"), Err(Unplayable::NoMovieBox));
        assert_eq!(checked(b"not a movie at all"), Err(Unplayable::NoMovieBox));
        // A box claiming to be shorter than its own header.
        let mut short = 4u32.to_be_bytes().to_vec();
        short.extend_from_slice(b"moovmoov");
        assert_eq!(checked(&short), Err(Unplayable::NoMovieBox));
    }

    #[test]
    fn a_stage_publishes_only_a_playable_movie_and_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("video.mp4");
        fs::write(&destination, RECORDING).unwrap();
        for (bytes, expected) in [
            (&b""[..], Err(Unplayable::Empty)),
            (b"media", Err(Unplayable::NoMovieBox)),
            (TRANSCODED, Ok(destination.clone())),
        ] {
            let stage = MovieStage::new(destination.clone()).unwrap();
            assert_eq!(stage.content().file_name(), destination.file_name());
            fs::write(stage.content(), bytes).unwrap();
            assert_eq!(stage.publish().unwrap(), expected);
            // A rejected movie leaves the earlier file in place; a published one replaces it.
            let now = fs::read(&destination).unwrap();
            assert_eq!(now, if expected.is_ok() { TRANSCODED } else { RECORDING });
            let entries: Vec<_> = fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            assert_eq!(entries, ["video.mp4"]);
        }
        let stage = MovieStage::new(destination.clone()).unwrap();
        fs::write(stage.content(), RECORDING).unwrap();
        stage.discard();
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
